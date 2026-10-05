#!/usr/bin/env bash
# Observe inherited context at real runner boundaries using disposable tools.
set -euo pipefail
native_repo=$(cd "$(dirname "$0")/../../.." && pwd)
source "$native_repo/scripts/native/common.sh"
if [ "$#" -ne 0 ]; then echo "usage: scripts/native/tests/environment.sh" >&2; exit 64; fi
native_test_root=${NATIVE_TEST_RUNNER_ROOT:-$native_repo}
native_test_root=$(cd "$native_test_root" && pwd)
case "$native_test_root" in
    "$native_repo"|"$native_repo/target/native-validation/"*) ;;
    *) echo "runner test source must stay inside this checkout" >&2; exit 64 ;;
esac
native_new_output "" environment
native_out=$NATIVE_RUN_DIR
mkdir -p "$native_out"/{tools,lib,foreign/objects,"work with spaces"}
printf 'foreign index sentinel\n' > "$native_out/foreign/index"
printf 'foreign ref sentinel\n' > "$native_out/foreign/refs"
printf 'foreign object sentinel\n' > "$native_out/foreign/objects/sentinel"
cksum "$native_out/foreign/index" "$native_out/foreign/refs" \
    "$native_out/foreign/objects/sentinel" > "$native_out/sentinels-before.txt"

native_context_names=(
    GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_CONFIG GIT_CONFIG_PARAMETERS
    GIT_CONFIG_COUNT GIT_OBJECT_DIRECTORY GIT_DIR GIT_WORK_TREE
    GIT_IMPLICIT_WORK_TREE GIT_GRAFT_FILE GIT_INDEX_FILE GIT_NO_REPLACE_OBJECTS
    GIT_REPLACE_REF_BASE GIT_PREFIX GIT_SHALLOW_FILE GIT_COMMON_DIR GIT_NAMESPACE
    GIT_CONFIG_KEY_0 GIT_CONFIG_VALUE_0 GIT_CONFIG_KEY_23 GIT_CONFIG_VALUE_23
    GIT_CEILING_DIRECTORIES GIT_DISCOVERY_ACROSS_FILESYSTEM GIT_CONFIG_GLOBAL
    GIT_AUTHOR_NAME GIT_FUTURE_REPOSITORY_CONTEXT HERDR_IDENTITY HERDR_SOCKET HERDR_LAUNCH
    CREW_IDENTITY CREW_SOCKET CREW_LAUNCH
)
printf '%s\n' "${native_context_names[@]}" > "$native_out/context-names.txt"
native_inherited=()
for name in "${native_context_names[@]}"; do
    native_inherited+=("$name=$native_out/foreign")
done
native_inherited+=(
    "GIT_CONFIG_COUNT=1" "GIT_CONFIG_KEY_0=core.worktree"
    "GIT_INDEX_FILE=$native_out/foreign/index"
    "NATIVE_RUST_TOOLCHAIN=1.93.1" "NATIVE_NIXPKGS=preserved nix input"
    "CARGO_TARGET_DIR=$native_out/caller-cargo"
    "LD_LIBRARY_PATH=preserved-parent-library"
    "QA_NATIVE_ENV_SENTINEL=preserved ordinary value"
    "QA_NATIVE_ENV_OUT=$native_out" "QA_NATIVE_ENV_NAMES=$native_out/context-names.txt"
    "QA_NATIVE_EXPECTED_CWD=$native_out/work with spaces"
    "QA_NATIVE_EXPECTED_CARGO=$native_out/caller-cargo"
    "QA_NATIVE_EXPECTED_LD=preserved-parent-library"
    "PATH=$native_out/tools:$PATH"
)

cat > "$native_out/assert-child" <<'CHILD'
#!/usr/bin/env bash
set -euo pipefail
while IFS= read -r name; do
    if declare -p "$name" >/dev/null 2>&1; then
        echo "inherited context reached child: $name" >&2; exit 1
    fi
done < "$QA_NATIVE_ENV_NAMES"
test "$NATIVE_RUST_TOOLCHAIN" = 1.93.1
test "$NATIVE_NIXPKGS" = 'preserved nix input'
test "$QA_NATIVE_ENV_SENTINEL" = 'preserved ordinary value'
test "$CARGO_TARGET_DIR" = "$QA_NATIVE_EXPECTED_CARGO"
test "$LD_LIBRARY_PATH" = "$QA_NATIVE_EXPECTED_LD"
test "$PWD" = "$QA_NATIVE_EXPECTED_CWD"
printf '%s\n' "$@" > "$QA_NATIVE_ENV_OUT/${QA_NATIVE_CASE}.args"
CHILD

cat > "$native_out/tools/cc" <<'TOOL'
#!/usr/bin/env bash
exit 0
TOOL
cat > "$native_out/tools/pkg-config" <<'TOOL'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
    --exists) test "${QA_NATIVE_PKG_READY:-0}" = 1 ;;
    --libs-only-L) printf '%s\n' "-L$QA_NATIVE_ENV_OUT/lib" ;;
    *) exit 1 ;;
esac
TOOL
cat > "$native_out/tools/nix-shell" <<'TOOL'
#!/usr/bin/env bash
set -euo pipefail
while IFS= read -r name; do
    if declare -p "$name" >/dev/null 2>&1; then
        echo "inherited context reached Nix bootstrap: $name" >&2; exit 1
    fi
done < "$QA_NATIVE_ENV_NAMES"
while [ "$#" -gt 0 ] && [ "$1" != --run ]; do shift; done
test "$#" = 2
# A shell hook can introduce context after the first boundary. Re-entry must
# remove it again before the requested build/test command.
export QA_NATIVE_PKG_READY=1 GIT_DIR="$QA_NATIVE_ENV_OUT/foreign"
export GIT_NAMESPACE=foreign HERDR_SOCKET=foreign CREW_LAUNCH=foreign
export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.worktree
export GIT_CONFIG_VALUE_0="$QA_NATIVE_ENV_OUT/foreign"
exec bash -c "$2"
TOOL
cat > "$native_out/tools/cargo" <<'TOOL'
#!/usr/bin/env bash
set -euo pipefail
exec "$QA_NATIVE_ENV_OUT/assert-child" "$@"
TOOL
chmod +x "$native_out/assert-child" "$native_out/tools/"*

cd "$native_out/work with spaces"
env "${native_inherited[@]}" QA_NATIVE_CASE=common bash -c \
    'source "$1/scripts/native/common.sh"; exec "$2/assert-child" "argument with spaces" "literal \$()"' \
    bash "$native_test_root" "$native_out"
env "${native_inherited[@]}" QA_NATIVE_CASE=linux-available QA_NATIVE_PKG_READY=1 \
    "QA_NATIVE_EXPECTED_LD=$native_out/lib:preserved-parent-library" \
    "$native_test_root/scripts/native/linux-env.sh" "$native_out/assert-child" \
    'argument with spaces' 'literal $()'
env "${native_inherited[@]}" QA_NATIVE_CASE=linux-reentry \
    "QA_NATIVE_EXPECTED_LD=$native_out/lib:preserved-parent-library" \
    "$native_test_root/scripts/native/linux-env.sh" "$native_out/assert-child" \
    'argument with spaces' 'literal $()'
env "${native_inherited[@]}" QA_NATIVE_CASE=linux-build QA_NATIVE_PKG_READY=1 \
    "QA_NATIVE_EXPECTED_LD=$native_out/lib:preserved-parent-library" \
    "QA_NATIVE_EXPECTED_CWD=$native_test_root" \
    "QA_NATIVE_EXPECTED_CARGO=$native_test_root/target/native-validation/cargo" \
    "$native_test_root/scripts/native/linux-build.sh" --config 'build.rustflags=["value with spaces"]'

printf '%s\n' 'argument with spaces' 'literal $()' > "$native_out/expected.args"
for result in common linux-available linux-reentry; do
    cmp "$native_out/expected.args" "$native_out/$result.args"
done
printf '%s\n' build --locked -p crew-desktop --features native-ui,local \
    --config 'build.rustflags=["value with spaces"]' > "$native_out/expected-build.args"
cmp "$native_out/expected-build.args" "$native_out/linux-build.args"
cksum "$native_out/foreign/index" "$native_out/foreign/refs" \
    "$native_out/foreign/objects/sentinel" > "$native_out/sentinels-after.txt"
cmp "$native_out/sentinels-before.txt" "$native_out/sentinels-after.txt"
echo "environment checks passed: common, Linux available/re-entry/build, caller args/environment, foreign sentinels"
echo "artifacts: $native_out"
