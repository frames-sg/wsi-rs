#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${LIBCLANG_PATH:-}" ]]; then
  libclang_dir=""
  for dir in /usr/lib/llvm-*/lib "${HOME}"/.local/llvm*/usr/lib/llvm-*/lib; do
    if compgen -G "${dir}/libclang*.so*" >/dev/null; then
      libclang_dir="${dir}"
    fi
  done
  test -n "${libclang_dir}" || {
    echo "libclang not found; CUDA Oxide bindgen cannot run" >&2
    exit 1
  }
  echo "LIBCLANG_PATH=${libclang_dir}" >> "${GITHUB_ENV}"
fi

resource_dir=""
for dir in /usr/lib/llvm-*/lib/clang/[0-9]* "${HOME}"/.local/llvm*/usr/lib/llvm-*/lib/clang/[0-9]*; do
  if [[ -d "${dir}/include" ]]; then
    resource_dir="${dir}"
  fi
done

bindgen_args="${BINDGEN_EXTRA_CLANG_ARGS:-}"
if [[ -n "${resource_dir}" ]]; then
  bindgen_args="${bindgen_args:+${bindgen_args} }-resource-dir=${resource_dir}"
fi
gcc_include="$(gcc -print-file-name=include)"
if [[ -d "${gcc_include}" ]]; then
  bindgen_args="${bindgen_args:+${bindgen_args} }-I${gcc_include}"
fi
if [[ -d /usr/local/cuda/include ]]; then
  bindgen_args="${bindgen_args:+${bindgen_args} }-I/usr/local/cuda/include"
fi
echo "BINDGEN_EXTRA_CLANG_ARGS=${bindgen_args}" >> "${GITHUB_ENV}"
