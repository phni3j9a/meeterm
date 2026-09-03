#!/usr/bin/env bash
set -u

readonly png_path="${1:?PNG path is required}"
readonly diagnostic_path="${2:?diagnostic path is required}"
readonly png_signature="89504e470d0a1a0a"

if [[ ! -s "${png_path}" ]]; then
  rm -f "${png_path}"
  echo "screenshot capture produced no image data" > "${diagnostic_path}"
  exit 0
fi

actual_signature="$(od -An -tx1 -N8 "${png_path}" | tr -d ' \n')"
if [[ "${actual_signature}" != "${png_signature}" ]]; then
  rm -f "${png_path}"
  echo "screenshot capture did not produce a valid PNG signature" > "${diagnostic_path}"
  exit 0
fi

mime_type="$(file -b --mime-type "${png_path}" 2>/dev/null || true)"
if [[ "${mime_type}" != "image/png" ]]; then
  rm -f "${png_path}"
  echo "screenshot capture was not recognized as an image/png file" > "${diagnostic_path}"
  exit 0
fi

rm -f "${diagnostic_path}"
