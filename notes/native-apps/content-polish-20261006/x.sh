#!/usr/bin/env bash
# x.sh OUT COMMAND...: run inside the private session's environment.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$1; shift
printf -v cmd '%q ' "$@"
exec nix-shell "$here/shell.nix" --run "source $out/session.env; export GI_TYPELIB_PATH=\$CAPTURE_GI_TYPELIB_PATH CAPTURES=$out/captures; cd $here; $cmd"
