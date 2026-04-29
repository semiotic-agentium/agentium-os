#!/usr/bin/env bash
# Emit LIBCLANG_PATH for bindgen / clang-sys (e.g. hirofa-quickjs-sys).
# On Ubuntu, libclang.so often lives under /usr/lib/llvm-*/lib without a reliably
# detected default path for clang_sys.
set -euo pipefail

shopt -s nullglob

libclang_dir() {
  local p
  local -a dirs=(/usr/lib/llvm-*/lib)
  for p in "${dirs[@]}"; do
    [[ -d "$p" ]] || continue
    if find "$p" -maxdepth 1 \( -name 'libclang.so' -o -name 'libclang.so.*' \) \
      -print -quit 2>/dev/null | grep -q .; then
      echo "$p"
      return 0
    fi
  done

  local d="/usr/lib/x86_64-linux-gnu"
  if [[ -d "$d" ]] && \
    find "$d" -maxdepth 1 \( -name 'libclang.so' -o -name 'libclang.so.*' \) \
      -print -quit 2>/dev/null | grep -q .; then
    echo "$d"
    return 0
  fi

  return 1
}

resolved=""
resolved="$(libclang_dir)" || {
  echo "error: could not locate libclang shared library; install libclang-dev clang llvm-dev" >&2
  find /usr/lib -maxdepth 3 -name 'libclang.so*' 2>/dev/null | head -20 >&2 || true
  exit 1
}

if [[ -z "${GITHUB_ENV:-}" ]] || [[ ! -f "${GITHUB_ENV}" ]]; then
  echo "LIBCLANG_PATH=$resolved"
  export LIBCLANG_PATH="$resolved"
  exit 0
fi

echo "LIBCLANG_PATH=$resolved" >>"$GITHUB_ENV"
echo "set LIBCLANG_PATH=$resolved (for bindgen/hirofa-quickjs-sys)"
