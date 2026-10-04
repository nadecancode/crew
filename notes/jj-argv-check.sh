#!/usr/bin/env bash
# Runs every jj invocation that notes/jj-design.md specifies against a real jj
# in a throwaway project, and checks what the design relies on. Needs jj >= 0.44
# (CREW_JJ=<path> or `jj` on PATH; `nix shell nixpkgs#jujutsu` provides one) and git.
set -euo pipefail

JJ=${CREW_JJ:-jj}
ver=$("$JJ" --version)
case "$ver" in "jj 0."[0-3]*|"jj 0.4"[0-3]*) echo "need jj >= 0.44, found: $ver" >&2; exit 1 ;; esac
echo "using $ver"

T=$(mktemp -d)
trap 'chmod -R u+w "$T" 2>/dev/null; rm -rf "$T"' EXIT
export JJ_CONFIG=/dev/null

# §ops "Invocation": the pinned global arguments, G.
G=(--no-pager --color never --quiet
   --config signing.behavior=drop
   --config signing.backend=none
   --config fsmonitor.backend=none
   --config 'ui.editor="true"'
   --config 'ui.diff-formatter=":git"'
   --config git.abandon-unreachable-commits=false
   --config 'snapshot.auto-track="all()"'
   --config 'snapshot.max-new-file-size="1TiB"'
   --config snapshot.auto-update-stale=false)
# jj as <identity>.
j() { local who=$1; shift; JJ_USER=$who JJ_EMAIL=$who@crew.local "$JJ" "${G[@]}" "$@"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
eq() { [ "$1" = "$2" ] || fail "$3: expected '$2', got '$1'"; }
ugit() { git -c user.name=u -c user.email=u@example.com "$@"; }

INFO='commit_id ++ "\n" ++ change_id ++ "\n" ++ description.first_line() ++ "\n" ++ self.diff().files().len() ++ "\n"'
STATE='conflict ++ "\n" ++ empty ++ "\n" ++ conflicted_files.map(|f| f.path()).join("\n") ++ "\n"'
DUP=$'templates.duplicate_description=\'description ++ "\\n(cherry picked from commit " ++ commit_id ++ ")\\n"\''

# ---------------------------------------------------------------- hidden layout
WS=$T/proj; R=$WS/.crew/jj; WT=$WS/.crew/worktrees
git init -q -b main "$WS"; cd "$WS"
printf 'hello\ntwo\n' > a.txt; echo world > b.txt
git add -A; ugit commit -q -m first
head_before=$(git rev-parse HEAD); index_before=$(git ls-files -s | sha1sum)
mkdir -p "$WT"; printf '*\n' > .crew/.gitignore            # keeps .crew out of git status and of jj snapshots

# ensure handle
j crew git init --git-repo "$WS" "$R"
j crew -R "$R" sparse set --clear
j crew -R "$R" workspace forget default
[ "$(ls -A "$R")" = ".jj" ] || fail "handle holds files"
j crew -R "$R" --ignore-working-copy git import

# resolve base HEAD: .git/HEAD names the branch, jj resolves the bookmark
eq "$(cat .git/HEAD)" "ref: refs/heads/main" "HEAD file"
BASE=$(j crew -R "$R" --ignore-working-copy log --no-graph -r 'present(bookmarks(exact:"main"))' -T 'commit_id ++ "\n"')
eq "$BASE" "$head_before" "base"

ensure_workspace() { # id base-revset
  local id=$1 base=$2
  if [ -z "$(j crew -R "$R" --ignore-working-copy log --no-graph -r "present(bookmarks(exact:\"agents/$id\"))" -T 'commit_id ++ "\n"')" ]; then
    j crew -R "$R" --ignore-working-copy bookmark create "agents/$id" -r "$base"
  fi
  if ! j crew -R "$R" --ignore-working-copy workspace list -T 'name ++ "\n"' | grep -qx "$id"; then
    j "$id" -R "$R" workspace add --name "$id" --sparse-patterns empty -r "\"agents/$id\"" "$WT/$id"
    printf 'gitdir: /nonexistent/crew-jj-workspace\n' > "$WT/$id/.git"
  fi
  j "$id" -R "$WT/$id" sparse reset                      # populate (helper sandbox)
  j crew -R "$R" --ignore-working-copy git export
}
for id in a b d; do ensure_workspace "$id" "commit_id($BASE)"; done
[ -f "$WT/a/a.txt" ] || fail "workspace not populated"
eq "$(git rev-parse refs/heads/agents/a)" "$BASE" "exported agent branch"
( cd "$WT/a" && ! git rev-parse --show-toplevel >/dev/null 2>&1 ) || fail "git guard does not stop discovery"

commit() { # id message
  local id=$1 msg=$2 w=$WT/$1
  local st; st=$(j "$id" -R "$w" log --no-graph -r @ -T 'empty ++ "\n" ++ parents.map(|p| p.commit_id()).join(",") ++ "\n"')
  [ "$(echo "$st" | sed -n 1p)" = false ] || { echo "nothing to commit"; return 1; }
  j "$id" -R "$w" commit "--message=$msg"
  j "$id" -R "$w" bookmark set "agents/$id" -r @-
  j "$id" -R "$w" git export
  j "$id" -R "$w" --ignore-working-copy log --no-graph -r @- -T "$INFO ++ if(self.contained_in(\"remote_bookmarks(exact:\\\"agents/$id\\\", exact:\\\"git\\\")\"), \"exported\", \"pending\") ++ \"\\n\""
}
printf 'hello\nA\n' > "$WT/a/a.txt"; echo n > "$WT/a/n.txt"
info=$(commit a "-a: change a.txt")
A1=$(echo "$info" | sed -n 1p)
eq "$(echo "$info" | sed -n 3p)" "-a: change a.txt" "subject"
eq "$(echo "$info" | sed -n 4p)" "2" "files changed"
eq "$(echo "$info" | sed -n 5p)" "exported" "export state"
eq "$(git rev-parse refs/heads/agents/a)" "$A1" "git sees the commit"
eq "$(git log -1 --format='%an <%ae>|%cn' agents/a)" "a <a@crew.local>|a" "identity"
git cat-file -p agents/a | grep -q '^change-id ' || fail "no change-id header"
commit a "again" >/dev/null && fail "empty commit was made" || true
! git ls-tree -r --name-only agents/a | grep -q '^\.git$\|^\.jj' || fail "guard or .jj committed"

# status and diff
echo more >> "$WT/a/b.txt"
j a -R "$WT/a" --ignore-working-copy log --no-graph -r @- -T 'bookmarks ++ "\n" ++ commit_id.short(8) ++ " " ++ description.first_line() ++ "\n"' | grep -q '^agents/a$' || fail "status header"
( cd "$WT/a" && eq "$(j a -R "$WT/a" diff --summary)" "M b.txt" "diff --summary" )
j a -R "$WT/a" diff --git | grep -q '^+more$' || fail "diff --git"
j a -R "$WT/a" diff --stat --from '"main"' --to @ | grep -q '3 files changed' || fail "diff --stat"
j a -R "$WT/a" diff --git --from "commit_id($BASE)" --to @ | grep -q '^diff --git a/n.txt b/n.txt$' || fail "diff --from"
commit a "a: more" >/dev/null

# other agents: a clean commit and a conflicting one
echo "b new" > "$WT/b/bnew.txt"; B1=$(commit b "b: add bnew" | sed -n 1p)
printf 'hello\nD\n' > "$WT/d/a.txt"; D1=$(commit d $'d: conflicting\n\nbody' | sed -n 1p)

head_of() { j "$1" -R "$WT/$1" --ignore-working-copy log --no-graph -r @- -T 'commit_id'; }
# step 1 of every state-changing operation: snapshot; is the working copy clean, and what is its parent
CLEAN='empty ++ "\n" ++ parents.map(|p| p.commit_id()).join(",") ++ "\n"'
clean() { eq "$(j "$1" -R "$WT/$1" log --no-graph -r @ -T "$CLEAN" | sed -n 1p)" true "$1 clean"; }

cherry_pick() { # id rev-set
  local id=$1 x=$2 w=$WT/$1
  clean "$id"
  local src; src=$(j "$id" -R "$w" --ignore-working-copy log --no-graph -r "$x" -T 'commit_id ++ " " ++ author.email() ++ "\n"')
  [ "$(echo "$src" | grep -c .)" = 1 ] || { echo "unknown commit"; return 2; }
  j "$id" -R "$w" --config "$DUP" duplicate "commit_id(${src%% *})" --insert-before @
  local st; st=$(j "$id" -R "$w" --ignore-working-copy log --no-graph -r @- -T "$STATE")
  if [ "$(echo "$st" | sed -n 1p)" = true ] || [ "$(echo "$st" | sed -n 2p)" = true ]; then
    j "$id" -R "$w" abandon @-
    echo "conflict: $(echo "$st" | sed -n '3,$p' | tr '\n' ' ')"; return 1
  fi
  j "$id" -R "$w" bookmark set "agents/$id" -r @-
  j "$id" -R "$w" git export
  echo "${src#* }"
}
before=$(head_of a)
eq "$(cherry_pick a "commit_id(${B1:0:10})")" "b@crew.local" "pick source author"
[ -f "$WT/a/bnew.txt" ] || fail "pick did not land"
j a -R "$WT/a" --ignore-working-copy log --no-graph -r @- -T description | grep -q "^(cherry picked from commit $B1)$" || fail "trailer"
eq "$(git log -1 --format='%an|%cn' agents/a)" "b|a" "pick keeps author"
before=$(head_of a)
out=$(cherry_pick a "commit_id($D1)") && fail "conflict not reported" || true
eq "$out" "conflict: a.txt " "conflicted files"
eq "$(head_of a)" "$before" "pick rollback: head"
eq "$(cat "$WT/a/a.txt")" "$(printf 'hello\nA')" "pick rollback: file"
clean a
eq "$(git rev-parse refs/heads/agents/a)" "$before" "pick rollback: branch"
out=$(cherry_pick a "commit_id($B1)") && fail "empty pick kept" || true     # already applied: empty, rolled back
eq "$(head_of a)" "$before" "empty pick rollback"
cherry_pick a 'commit_id(deadbeefdeadbeef)' >/dev/null && fail "unknown commit accepted" || true
CH=$(j a -R "$WT/a" --ignore-working-copy log --no-graph -r "commit_id($D1)" -T change_id)
eq "$(j a -R "$WT/a" --ignore-working-copy log --no-graph -r "change_id(${CH:0:12})" -T commit_id)" "$D1" "change id lookup"

merge() { # workspace-dir identity branch
  local w=$1 who=$2 br=$3
  local st; st=$(j "$who" -R "$w" log --no-graph -r @ -T "$CLEAN")
  eq "$(echo "$st" | sed -n 1p)" true "clean before merge"
  local p; p=$(echo "$st" | sed -n 2p)
  [ -n "$(j "$who" -R "$w" --ignore-working-copy log --no-graph -r "present(bookmarks(exact:\"$br\"))" -T 'commit_id ++ "\n"')" ] || { echo missing; return 3; }
  [ -n "$(j "$who" -R "$w" --ignore-working-copy log --no-graph -r "\"$br\" ~ ::@-" -T '"x\n"')" ] || { echo up-to-date; return 0; }
  j "$who" -R "$w" new --no-edit "--message=Merge $br" --insert-after @- --insert-after "\"$br\"" --insert-before @
  st=$(j "$who" -R "$w" --ignore-working-copy log --no-graph -r @- -T "$STATE")
  if [ "$(echo "$st" | sed -n 1p)" = true ]; then
    local m; m=$(j "$who" -R "$w" --ignore-working-copy log --no-graph -r @- -T commit_id)
    j "$who" -R "$w" rebase -r @ --onto "commit_id($p)"
    j "$who" -R "$w" abandon "commit_id($m)"
    echo "conflict: $(echo "$st" | sed -n '3,$p' | tr '\n' ' ')"; return 1
  fi
  echo merged
}
before=$(head_of b)
eq "$(merge "$WT/b" b agents/a)" merged "merge"
j b -R "$WT/b" bookmark set agents/b -r @-; j b -R "$WT/b" git export
eq "$(git log -1 --format='%s|%an|%P' agents/b | cut -d'|' -f1,2)" "Merge agents/a|b" "merge commit"
eq "$(git rev-list --parents -1 agents/b | wc -w)" 3 "two parents"
eq "$(merge "$WT/b" b agents/a)" up-to-date "second merge"
before=$(head_of b)
out=$(merge "$WT/b" b agents/d) && fail "merge conflict not reported" || true
eq "$out" "conflict: a.txt " "merge conflicted files"
eq "$(head_of b)" "$before" "merge rollback: head"; clean b
eq "$(cat "$WT/b/a.txt")" "$(printf 'hello\nA')" "merge rollback: file"
merge "$WT/b" b agents/nobody >/dev/null && fail "missing branch merged" || true

# integration
j crew -R "$R" --ignore-working-copy bookmark create agents/integration -r "commit_id($BASE)"
j crew -R "$R" workspace add --name _integration --sparse-patterns empty -r '"agents/integration"' "$WT/_integration"
j crew -R "$WT/_integration" sparse reset
I=$WT/_integration; merged=""; conflicted=""
for id in a b d ghost; do
  out=$(merge "$I" crew "agents/$id") && rc=0 || rc=$?
  case "$out" in
    merged) j crew -R "$I" bookmark set agents/integration -r @-; merged="$merged $id" ;;
    conflict*) conflicted="$conflicted $id" ;;
  esac
done
j crew -R "$I" git export
eq "$merged" " a b" "integration merged"; eq "$conflicted" " d" "integration conflicts"
[ -f "$I/bnew.txt" ] && [ -f "$I/n.txt" ] || fail "integration tree"
eq "$(git log -1 --format='%an' agents/integration)" "crew" "integration identity"

# bookmark never moves sideways
j a -R "$WT/a" bookmark set agents/a -r '"main"' 2>/dev/null && fail "sideways move allowed" || true

# stale workspace: an error, not an automatic update
echo inj > "$WT/d/inj.txt"
j d -R "$WT/d" squash --from @ --into '"agents/a"' -u
out=$(j a -R "$WT/a" status 2>&1) && fail "stale workspace accepted" || true
echo "$out" | grep -q "working copy is stale" || fail "stale not reported"
j a -R "$WT/a" workspace update-stale
j a -R "$WT/a" status >/dev/null

# timeout recovery command is a no-op on a healthy workspace
j b -R "$WT/b" workspace update-stale

# a workspace directory removed by hand
rm -rf "$WT/d"
j crew -R "$R" --ignore-working-copy workspace forget d
j d -R "$R" workspace add --name d --sparse-patterns empty -r '"agents/d"' "$WT/d"
j d -R "$WT/d" sparse reset
[ -f "$WT/d/a.txt" ] || fail "re-added workspace"

# the store works with the read-only top levels the sandbox mounts (§sandbox):
# everything an agent's or the helper's jj does, with nothing creatable beside the store
j crew -R "$R" --ignore-working-copy bookmark create agents/e -r "commit_id($BASE)"
j e -R "$R" workspace add --name e --sparse-patterns empty -r '"agents/e"' "$WT/e"
chmod a-w "$R/.jj/repo" "$R/.jj/repo/store" "$WT/b/.jj" "$WT/e/.jj" .git
touch "$R/.jj/repo/config.toml" 2>/dev/null && fail "repo top level writable" || true
touch "$WT/e/.jj/workspace-config.toml" 2>/dev/null && fail "workspace .jj writable" || true
j e -R "$WT/e" sparse reset
[ -f "$WT/e/a.txt" ] || fail "populate with read-only store"
echo ro > "$WT/b/ro.txt"; commit b "b: with read-only store top level" >/dev/null
eq "$(cherry_pick e "commit_id($B1)")" "b@crew.local" "pick with read-only store"
eq "$(merge "$WT/e" e agents/a)" merged "merge with read-only store"
j e -R "$WT/e" bookmark set agents/e -r @-; j e -R "$WT/e" git export
out=$(merge "$WT/e" e agents/d) && fail "conflict with read-only store" || true
eq "$out" "conflict: a.txt " "rollback with read-only store"; clean e
chmod u+w "$R/.jj/repo" "$R/.jj/repo/store" "$WT/b/.jj" "$WT/e/.jj" .git

# the user's checkout was never touched
eq "$(git rev-parse HEAD)" "$head_before" "user HEAD"
eq "$(cat .git/HEAD)" "ref: refs/heads/main" "user HEAD ref"
eq "$(git ls-files -s | sha1sum)" "$index_before" "user index"
eq "$(git status --porcelain)" "" "user working tree"
[ ! -e "$WS/.jj" ] || fail ".jj in the project root"
find "$WS" -name '*.lock' | grep -q . && fail "lock files left" || true

# ---------------------------------------------------------------- native layout
NS=$T/native; mkdir -p "$NS"; cd "$NS"
git init -q -b main .; echo hi > a.txt; git add -A; ugit commit -q -m first
JJ_USER=u JJ_EMAIL=u@example.com "$JJ" --quiet git init --colocate
mkdir -p .crew/worktrees; printf '*\n' > .crew/.gitignore; echo dirty > dirty.txt
# bootstrap: the user's own config applies to the snapshot of their working copy
( unset JJ_CONFIG; JJ_USER=crew JJ_EMAIL=crew@crew.local "$JJ" --no-pager --color never --quiet \
    --config signing.behavior=drop --config signing.backend=none --config fsmonitor.backend=none --config 'ui.editor="true"' \
    -R "$NS" workspace add --name crew --sparse-patterns empty -r 'root()' "$NS/.crew/jj" )
j crew -R "$NS/.crew/jj" --ignore-working-copy workspace forget crew
NBASE=$(j crew -R "$NS" --ignore-working-copy log --no-graph -r @- -T 'commit_id ++ "\n"')
eq "$NBASE" "$(git rev-parse main)" "native base"
j crew -R "$NS/.crew/jj" --ignore-working-copy git root | grep -q "/native/.git$" || fail "git root"
j crew -R "$NS/.crew/jj" --ignore-working-copy bookmark create agents/a -r "commit_id($NBASE)"
j a -R "$NS/.crew/jj" workspace add --name a --sparse-patterns empty -r '"agents/a"' "$NS/.crew/worktrees/a"
j a -R "$NS/.crew/worktrees/a" sparse reset
j crew -R "$NS/.crew/jj" --ignore-working-copy git export
eq "$(git rev-parse refs/heads/agents/a)" "$NBASE" "native export"
JJ_USER=u JJ_EMAIL=u@example.com "$JJ" --no-pager --color never status | grep -q '\.crew' && fail "user's jj sees .crew" || true

echo "PASS: every argv of notes/jj-design.md ran on $ver"
