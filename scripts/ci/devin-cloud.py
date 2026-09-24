#!/usr/bin/env python3
"""Drive Devin Cloud validation sessions through `devin acp --cloud`.

The v3 Sessions API cannot select SWE-2, but the Devin CLI's cloud ACP relay
exposes the session's agent version and VM platform as config options. This
helper creates SWE-2 sessions, sends prompts to existing sessions, and reports
their status using the CLI's own login (no DEVIN_API_KEY). See
docs/CI_MOBILE.md.
"""
import argparse
import json
import queue
import subprocess
import sys
import threading
import time

DEFAULT_REPO = 'phni3j9a/meeterm'
DEFAULT_VERSION = 'devin-swe-2-max'
PLATFORMS = ('linux', 'macos')
META = 'cognition.ai/'


class AcpError(RuntimeError):
    pass


def option_values(config_option):
    values = []
    for option in config_option.get('options', []):
        values += [nested['value'] for nested in option.get('options', [])] if 'options' in option else [option['value']]
    return values


def require_option(config_options, config_id, value):
    for config_option in config_options:
        if config_option['id'] == config_id:
            if value not in option_values(config_option):
                raise AcpError(f'{config_id}={value!r} is not offered; available: {option_values(config_option)}')
            return
    raise AcpError(f'config option {config_id!r} is not offered by this Devin Cloud relay')


def current_values(config_options):
    return {config_option['id']: config_option.get('currentValue') for config_option in config_options}


def session_row(session):
    meta = session.get('_meta', {})
    return {
        'session_id': session['sessionId'],
        'title': session.get('title', ''),
        'status': meta.get(META + 'sessionStatus'),
        'platform': meta.get(META + 'platform') or 'linux',
        'version': meta.get(META + 'devinVersionOverride'),
        'archived': bool(meta.get(META + 'isArchived')),
        'url': meta.get(META + 'url'),
    }


def full_session_id(session_id):
    return session_id if session_id.startswith('devin-') else 'devin-' + session_id


class Client:
    def __init__(self, cwd):
        self.cwd = cwd
        self.process = subprocess.Popen(['devin', 'acp', '--cloud'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, text=True, bufsize=1)
        self.lines = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.next_id = 0
        self.messages = []
        self.call('initialize', {'protocolVersion': 1, 'clientCapabilities': {}})

    def _read(self):
        for line in self.process.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def _write(self, message):
        self.process.stdin.write(json.dumps(message) + '\n')
        self.process.stdin.flush()

    def _handle(self, message):
        if message.get('method') == 'session/update':
            update = message['params'].get('update', {})
            if update.get('sessionUpdate') == 'agent_message_chunk':
                self.messages.append(update.get('content', {}).get('text', ''))
        elif 'method' in message and 'id' in message:
            self._write({'jsonrpc': '2.0', 'id': message['id'],
                         'error': {'code': -32601, 'message': 'not supported by scripts/ci/devin-cloud.py'}})

    def drain(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            try:
                line = self.lines.get(timeout=0.5)
            except queue.Empty:
                continue
            if line is None:
                return
            self._handle(json.loads(line))

    def call(self, method, params, timeout=120):
        self.next_id += 1
        request_id = self.next_id
        self._write({'jsonrpc': '2.0', 'id': request_id, 'method': method, 'params': params})
        end = None if timeout is None else time.time() + timeout
        while end is None or time.time() < end:
            try:
                line = self.lines.get(timeout=0.5)
            except queue.Empty:
                continue
            if line is None:
                raise AcpError(f'devin acp --cloud exited during {method}')
            message = json.loads(line)
            if message.get('id') == request_id and ('result' in message or 'error' in message):
                if 'error' in message:
                    raise AcpError(f'{method}: {message["error"]}')
                return message['result']
            self._handle(message)
        return None

    def list(self, include_archived=False):
        params = {'cwd': self.cwd}
        if include_archived:
            params['_meta'] = {META + 'archivedStatus': 'ALL'}
        return [session_row(session) for session in self.call('session/list', params)['sessions']]

    def row(self, session_id):
        for row in self.list(include_archived=True):
            if row['session_id'] == session_id:
                return row
        raise AcpError(f'{session_id} is not listed')

    def create(self, repo, version, platform):
        session = self.call('session/new', {'cwd': self.cwd, 'mcpServers': []})
        session_id = session['sessionId']
        config_options = session['configOptions']
        for config_id, value in (('repos', repo), ('devin_version', version), ('platform', platform)):
            require_option(config_options, config_id, value)
            config_options = self.call('session/set_config_option',
                                       {'sessionId': session_id, 'configId': config_id, 'value': value})['configOptions']
        applied = current_values(config_options)
        expected = {'repos': repo, 'devin_version': version, 'platform': platform}
        if any(applied.get(key) != value for key, value in expected.items()):
            raise AcpError(f'config not applied: {applied}')
        return session_id

    def load(self, session_id):
        self.messages = []
        self.call('session/load', {'sessionId': session_id, 'cwd': self.cwd, 'mcpServers': []})
        self.drain(3)
        replayed, self.messages = self.messages, []
        return replayed

    def prompt(self, session_id, text, wait):
        result = self.call('session/prompt', {'sessionId': session_id, 'prompt': [{'type': 'text', 'text': text}]},
                           timeout=wait)
        if result is not None:
            self.drain(5)
        return result

    def close(self):
        self.process.terminate()


def read_prompt(args):
    if args.prompt_file:
        with open(args.prompt_file, encoding='utf-8') as handle:
            return handle.read()
    if args.prompt:
        return args.prompt
    raise SystemExit('--prompt or --prompt-file is required')


def remote_head(cwd, branch):
    """Return the evidence branch head on origin, or '' when it does not exist yet."""
    completed = subprocess.run(['git', 'ls-remote', 'origin', f'refs/heads/{branch}'], cwd=cwd,
                               capture_output=True, text=True, timeout=60)
    if completed.returncode != 0:
        raise AcpError(f'git ls-remote failed for {branch}: {completed.stderr.strip()}')
    return completed.stdout.split('\t', 1)[0].strip()


def wait_evidence(cwd, branch, after, timeout, interval, head=remote_head, sleep=time.sleep, clock=time.monotonic):
    """Block until the session pushes a new evidence commit.

    Validation runs push their observability bundle even on failure, so a new
    head is the completion signal. Session status is not: an idle cloud session
    still reports 'running'.
    """
    baseline = head(cwd, branch) if after is None else after
    end = clock() + timeout
    while True:
        try:
            current = head(cwd, branch)
        except (AcpError, subprocess.TimeoutExpired) as error:
            print('warning:', error, file=sys.stderr)
            current = baseline
        if current and current != baseline:
            return current
        remaining = end - clock()
        if remaining <= 0:
            return None
        sleep(min(interval, remaining))


def report_turn(client, session_id, result):
    if result is None:
        print('turn: still running (detached; the cloud session continues)')
    else:
        print('turn:', result.get('stopReason'))
    for text in client.messages:
        print('devin:', text)
    row = client.row(session_id)
    print(json.dumps(row, ensure_ascii=False))
    return row


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--cwd', default='.')
    commands = parser.add_subparsers(dest='command', required=True)
    listing = commands.add_parser('list', help='list cloud sessions')
    listing.add_argument('--all', action='store_true', help='include archived sessions')
    status = commands.add_parser('status', help='show one session and replay its recent Devin messages')
    status.add_argument('session_id')
    status.add_argument('--messages', type=int, default=3)
    for name in ('new', 'send'):
        command = commands.add_parser(name)
        command.add_argument('--prompt')
        command.add_argument('--prompt-file')
        command.add_argument('--wait', type=float, default=60,
                             help='seconds to wait for the turn before detaching (0 = do not wait)')
    commands.choices['new'].add_argument('--platform', choices=PLATFORMS, required=True)
    commands.choices['new'].add_argument('--version', default=DEFAULT_VERSION)
    commands.choices['new'].add_argument('--repo', default=DEFAULT_REPO)
    commands.choices['send'].add_argument('session_id')
    evidence = commands.add_parser('wait-evidence', help='wait until an evidence branch receives a new commit')
    evidence.add_argument('branch', help='for example evidence/ios-20260923')
    evidence.add_argument('--after', help='head to wait past (default: the head when waiting starts)')
    evidence.add_argument('--timeout', type=float, default=3600)
    evidence.add_argument('--interval', type=float, default=60)
    args = parser.parse_args(argv)

    if args.command == 'wait-evidence':
        try:
            new_head = wait_evidence(args.cwd, args.branch, args.after, args.timeout, args.interval)
        except AcpError as error:
            print('error:', error, file=sys.stderr)
            return 1
        if new_head is None:
            print(f'timeout: {args.branch} did not move within {args.timeout:g}s', file=sys.stderr)
            return 2
        print(f'evidence: {args.branch} {new_head}')
        return 0

    client = Client(args.cwd)
    try:
        if args.command == 'list':
            for row in client.list(args.all):
                print(json.dumps(row, ensure_ascii=False))
        elif args.command == 'status':
            session_id = full_session_id(args.session_id)
            replayed = client.load(session_id)
            for text in replayed[-args.messages:] if args.messages else []:
                print('devin:', text)
            print(json.dumps(client.row(session_id), ensure_ascii=False))
        elif args.command == 'new':
            text = read_prompt(args)
            session_id = client.create(args.repo, args.version, args.platform)
            print('session:', session_id)
            row = report_turn(client, session_id, client.prompt(session_id, text, args.wait or 1))
            if row['version'] != args.version:
                raise AcpError(f'session reports {row["version"]!r}, expected {args.version!r}')
        else:
            session_id = full_session_id(args.session_id)
            text = read_prompt(args)
            client.load(session_id)
            report_turn(client, session_id, client.prompt(session_id, text, args.wait or 1))
    except AcpError as error:
        print('error:', error, file=sys.stderr)
        return 1
    finally:
        client.close()
    return 0


if __name__ == '__main__':
    sys.exit(main())
