"""Browser-only mock checks. No SSH, native app, or real Codex validation.

Run: python test_mock.py
Requires Python Playwright and Chromium. Set CHROMIUM_PATH for a system browser.
The HTML is injected into a fresh blank page; no file:// permission is required.
"""
from pathlib import Path
import json
import os
import shutil
from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parent
HTML = (ROOT / 'index.html').read_text(encoding='utf-8')
EVIDENCE = Path(os.environ.get('MOCK_EVIDENCE_DIR', ROOT / 'evidence'))
EVIDENCE.mkdir(parents=True, exist_ok=True)
results, errors, requests = [], [], []


def check(name, condition=True):
    assert condition, name
    results.append(name)


def action(page, name, **attrs):
    selector = f'[data-action="{name}"]'
    for key, value in attrs.items():
        selector += f'[data-{key}="{value}"]'
    page.locator(selector).first.click()


def navigate(page, route):
    page.evaluate('(route) => { location.hash = route; }', route)
    expect(page.locator('h1').last).to_be_visible()
    page.wait_for_timeout(40)


def enter_terminal(page, workspace='meeterm'):
    action(page, 'connect', id='home')
    action(page, 'open-workspace', id=workspace)
    expect(page.locator('[role=tabpanel]')).to_be_visible()


with sync_playwright() as pw:
    executable = os.environ.get('CHROMIUM_PATH') or shutil.which('chromium')
    launch = {'headless': True}
    if executable:
        launch['executable_path'] = executable
    browser = pw.chromium.launch(**launch)

    def fresh(width=393, height=852, scale=1):
        page = browser.new_page(viewport={'width': width, 'height': height}, device_scale_factor=scale)
        page.on('pageerror', lambda e: errors.append(str(e)))
        page.on('request', lambda request: requests.append(request.url))
        page.set_content(HTML)
        expect(page.locator('#app h1')).to_have_text('いつもの場所へ。')
        return page

    page = fresh()
    action(page, 'connect', id='home')
    expect(page.locator('.workspace')).to_have_count(3)
    check('SSH destination leads to workspace list, with no session picker')
    action(page, 'open-workspace', id='meeterm')
    expect(page.get_by_role('tab')).to_have_count(3)
    check('Workspace opens three named, status-bearing pane tabs')
    page.get_by_label('デモコマンド', exact=True).fill('日本語の下書き')
    action(page, 'select-pane', id='p2')
    page.get_by_label('デモコマンド', exact=True).fill('pwd')
    page.get_by_label('デモ入力を送信').click()
    expect(page.locator('[role=tabpanel]')).to_contain_text('/home/dev/projects/meeterm')
    action(page, 'select-pane', id='p1')
    expect(page.get_by_label('デモコマンド', exact=True)).to_have_value('日本語の下書き')
    check('Japanese drafts and sample command history are isolated by pane')
    action(page, 'back-workspaces')
    action(page, 'open-workspace', id='meeterm')
    expect(page.get_by_label('デモコマンド', exact=True)).to_have_value('日本語の下書き')
    check('Back navigation preserves pane state')
    action(page, 'add-pane')
    page.locator('#new-name').fill('Logs')
    page.locator('#name-form button[type=submit]').click()
    expect(page.get_by_role('tab')).to_have_count(4)
    expect(page.locator('[aria-selected=true]')).to_contain_text('Logs')
    action(page, 'close-pane')
    page.get_by_role('button', name='キャンセル', exact=True).click()
    expect(page.get_by_role('tab')).to_have_count(4)
    action(page, 'close-pane')
    action(page, 'confirm-close')
    expect(page.get_by_role('tab')).to_have_count(3)
    check('Add pane, cancel close, and confirm close behave independently')
    action(page, 'back-workspaces')
    action(page, 'open-workspace', id='sekirei')
    action(page, 'close-pane')
    expect(page.locator('#sheet')).to_contain_text('最後のpane')
    action(page, 'confirm-close')
    expect(page.locator('.workspace')).to_have_count(2)
    check('Closing the last pane removes its workspace and returns to the list')
    action(page, 'open-workspace', id='rfkit')
    action(page, 'back-workspaces')
    expect(page.locator('.workspace.last')).to_contain_text('rfkit-rs')
    check('Most-recent-workspace indication follows user selection')
    action(page, 'open-workspace', id='meeterm')
    action(page, 'select-pane', id='p2')
    page.locator('[aria-selected=true]').press('ArrowRight')
    expect(page.locator('[aria-selected=true]')).to_contain_text('Tests')
    page.locator('[aria-selected=true]').press('Home')
    expect(page.locator('[aria-selected=true]')).to_contain_text('Codex')
    check('Pane tabs support Arrow keys and Home/End navigation')
    action(page, 'keyboard')
    expect(page.locator('.keyboard')).to_be_visible()
    action(page, 'key', value='space')
    expect(page.get_by_label('デモコマンド', exact=True)).to_have_value('日本語の下書き ')
    action(page, 'special', value='escape')
    expect(page.locator('.keyboard')).to_have_count(0)
    check('Sample keyboard and Esc work without a native-IME claim')
    navigate(page, 'settings')
    action(page, 'simulate-disconnect')
    expect(page.locator('.reconnect-banner')).to_be_visible()
    expect(page.get_by_label('デモコマンド', exact=True)).to_be_disabled()
    expect(page.get_by_label('新規paneを追加')).to_be_disabled()
    action(page, 'reconnect')
    expect(page.locator('.reconnect-banner')).to_have_count(0)
    expect(page.get_by_label('デモコマンド', exact=True)).to_have_value('日本語の下書き ')
    check('Disconnect disables mutation/input; reconnect preserves the workspace and draft')
    navigate(page, 'settings')
    action(page, 'font', value='1')
    check('Font-size setting updates the shared terminal CSS token', page.evaluate("getComputedStyle(document.documentElement).getPropertyValue('--term-size').trim()") == '13px')
    action(page, 'reset')
    action(page, 'confirm-reset')
    expect(page.locator('.host-card')).to_have_count(2)
    check('Confirmed reset restores the initial in-memory fixture')
    action(page, 'add-host')
    expect(page.locator('#sheet')).to_be_visible()
    page.keyboard.press('Escape')
    expect(page.locator('#sheet')).not_to_be_visible()
    check('Native dialog Escape cancellation works')
    action(page, 'add-host')
    page.locator('#host-name').fill('Lab test')
    page.locator('#host-address').fill('test.example')
    page.locator('#host-form button[type=submit]').click()
    page.locator('.host-card').filter(has_text='Lab test').click()
    expect(page.locator('#sheet')).to_contain_text('SHA256:DEMO')
    page.get_by_role('button', name='キャンセル', exact=True).click()
    expect(page.locator('.host-card')).to_have_count(3)
    page.locator('.host-card').filter(has_text='Lab test').click()
    action(page, 'trust-host')
    expect(page.locator('.empty')).to_be_visible()
    check('A new host requires explicit demo host-key confirmation and starts empty')
    action(page, 'add-workspace')
    page.locator('#new-name').fill('   ')
    page.locator('#name-form button[type=submit]').click()
    expect(page.locator('#sheet')).to_be_visible()
    page.locator('#new-name').fill('<img src=x onerror=alert(1)>')
    page.locator('#name-form button[type=submit]').click()
    expect(page.locator('#app h1')).to_have_text('<img src=x onerror=alert(1)>')
    check('Whitespace-only names are rejected and HTML-like names render as text', page.locator('img').count() == 0)
    page.close()

    for width, height in [(320, 680), (375, 812), (393, 852), (430, 932), (768, 900), (1280, 980)]:
        page = fresh(width, height)
        for screen in ('servers', 'workspaces', 'terminal', 'settings'):
            if screen == 'workspaces':
                action(page, 'connect', id='home')
            elif screen == 'terminal':
                action(page, 'open-workspace', id='meeterm')
            elif screen == 'settings':
                navigate(page, 'settings')
            check(f'No document-level horizontal overflow: {screen} at {width}x{height}', page.evaluate('document.documentElement.scrollWidth <= innerWidth'))
        page.close()

    page = fresh(scale=2)
    page.screenshot(path=str(EVIDENCE / '01-servers.png'))
    action(page, 'connect', id='home')
    page.screenshot(path=str(EVIDENCE / '02-workspaces.png'))
    action(page, 'open-workspace', id='meeterm')
    page.screenshot(path=str(EVIDENCE / '03-terminal.png'))
    action(page, 'keyboard')
    page.screenshot(path=str(EVIDENCE / '04-keyboard.png'))
    navigate(page, 'settings')
    page.screenshot(path=str(EVIDENCE / '05-settings.png'))
    action(page, 'simulate-disconnect')
    page.screenshot(path=str(EVIDENCE / '06-reconnect.png'))
    action(page, 'reconnect')
    action(page, 'close-pane')
    page.screenshot(path=str(EVIDENCE / '07-close-confirmation.png'))
    page.close()
    page = fresh(1280, 980, 1.5)
    page.screenshot(path=str(EVIDENCE / '00-desktop.png'))
    page.close()
    check('No browser JavaScript errors', not errors)
    check('No network requests from the mock', not requests)
    browser.close()

report = {'passed': len(results), 'checks': results, 'browser_errors': errors, 'network_requests': requests, 'scope': 'Chromium browser mock only; not Android/iOS/native/SSH validation'}
(EVIDENCE / 'test-results.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
print(json.dumps(report, ensure_ascii=False, indent=2))
