#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: meeterm iOS Rust builds require macOS with Xcode" >&2
  exit 2
fi

: "${BUILT_PRODUCTS_DIR:?Xcode must provide BUILT_PRODUCTS_DIR}"
: "${DERIVED_FILE_DIR:?Xcode must provide DERIVED_FILE_DIR}"
: "${PLATFORM_NAME:?Xcode must provide PLATFORM_NAME}"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
core_dir="$(cd "${script_dir}/../../../native/meeterm-core" && pwd)"
cargo_bin="${CARGO:-cargo}"
deployment_target="${IPHONEOS_DEPLOYMENT_TARGET:-16.4}"
configuration="${CONFIGURATION:-Debug}"
architectures="${ARCHS:-${CURRENT_ARCH:-}}"

if [[ -z "${architectures}" ]]; then
  echo "error: Xcode did not provide ARCHS or CURRENT_ARCH" >&2
  exit 2
fi

profile="debug"
cargo_profile_args=()
if [[ "${MEETERM_RUST_PROFILE:-}" == "release" || "${configuration}" == "Release" ]]; then
  profile="release"
  cargo_profile_args=(--release)
fi

target_dir="${DERIVED_FILE_DIR}/meeterm-rust-target"
archives=()
for architecture in ${architectures}; do
  case "${PLATFORM_NAME}:${architecture}" in
    iphoneos:arm64)
      rust_target="aarch64-apple-ios"
      ;;
    iphonesimulator:arm64)
      rust_target="aarch64-apple-ios-sim"
      ;;
    iphonesimulator:x86_64)
      rust_target="x86_64-apple-ios"
      ;;
    *)
      echo "error: unsupported iOS platform/architecture ${PLATFORM_NAME}/${architecture}" >&2
      exit 2
      ;;
  esac

  (
    # Rustup discovers rust-toolchain.toml from the process working directory,
    # not from Cargo's --manifest-path argument.
    cd "${core_dir}"
    CARGO_TARGET_DIR="${target_dir}" \
      IPHONEOS_DEPLOYMENT_TARGET="${deployment_target}" \
      "${cargo_bin}" build \
        --locked \
        --lib \
        --target "${rust_target}" \
        "${cargo_profile_args[@]}"
  )
  archive="${target_dir}/${rust_target}/${profile}/libmeeterm_core.a"
  if [[ ! -f "${archive}" ]]; then
    echo "error: Cargo did not produce ${archive}" >&2
    exit 2
  fi
  archives+=("${archive}")
done

mkdir -p "${BUILT_PRODUCTS_DIR}"
output="${BUILT_PRODUCTS_DIR}/libmeeterm_core.a"
temporary_output="${BUILT_PRODUCTS_DIR}/.libmeeterm_core.$$.a"
trap 'rm -f "${temporary_output}"' EXIT

if [[ ${#archives[@]} -eq 1 ]]; then
  cp "${archives[0]}" "${temporary_output}"
else
  xcrun lipo -create "${archives[@]}" -output "${temporary_output}"
fi
mv -f "${temporary_output}" "${output}"
