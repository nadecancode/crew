#!/usr/bin/env bash
# Runs every jj invocation that notes/jj-design.md specifies against a real jj
# in throwaway projects, and checks what the design relies on. Needs jj >= 0.44
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
line() { sed -n "$1p"; }

# §ops "Templates".
INFO='commit_id ++ "\n" ++ change_id ++ "\n" ++ description.first_line() ++ "\n" ++ self.diff().files().len() ++ "\n"'
STATE='commit_id ++ "\n" ++ conflict ++ "\n" ++ empty ++ "\n" ++ local_bookmarks.map(|b| b.name()).join(" ") ++ "\n" ++ conflicted_files.map(|f| f.path()).join("\n") ++ "\n"'
PRE='empty ++ "\n" ++ parents.map(|p| p.commit_id()).join(",") ++ "\n"'
DUP=$'templates.duplicate_description=\'description ++ "\\n(cherry picked from commit " ++ commit_id ++ ")\\n"\''

# All functions below work on $R (the handle) and $WT (the workspaces directory).

# §ops "Prepare", hidden layout. $1: the backing (common) git directory.
prepare() {
  j crew git init --git-repo "$1" "$R"
  j crew -R "$R" sparse set --clear
  j crew -R "$R" workspace forget default
  [ "$(ls -A "$R")" = ".jj" ] || fail "handle holds files"
  j crew -R "$R" --ignore-working-copy git import
}
bookmark() { j crew -R "$R" --ignore-working-copy log --no-graph -r "present(bookmarks(exact:\"$1\"))" -T 'commit_id ++ "\n"'; }

# §ops "Workspace": add_workspace, then populate.
ensure_workspace() { # id base-revset [identity]
  local id=$1 base=$2 who=${3:-$1}
  [ -n "$(bookmark "agents/$id")" ] || j crew -R "$R" --ignore-working-copy bookmark create "agents/$id" -r "$base"
  if ! j crew -R "$R" --ignore-working-copy workspace list -T 'name ++ "\n"' | grep -qx -- "$id"; then
    j "$who" -R "$R" workspace add --name "$id" --sparse-patterns empty -r "\"agents/$id\"" "$WT/$id"
    printf 'gitdir: /nonexistent/crew-jj-workspace\n' > "$WT/$id/.git"
  fi
  j "$who" -R "$WT/$id" sparse reset                     # populate (helper sandbox)
  j crew -R "$R" --ignore-working-copy git export
}

# §ops "Commit".
commit() { # id message
  local id=$1 msg=$2 w=$WT/$1
  [ "$(j "$id" -R "$w" log --no-graph -r @ -T "$PRE" | line 1)" = false ] || { echo "nothing to commit"; return 1; }
  j "$id" -R "$w" commit "--message=$msg"
  publish "$w" "$id" "agents/$id"
  j "$id" -R "$w" --ignore-working-copy log --no-graph -r @- -T "$INFO ++ if(self.contained_in(\"remote_bookmarks(exact:\\\"agents/$id\\\", exact:\\\"git\\\")\"), \"exported\", \"pending\") ++ \"\\n\""
}
# The publish phase: the bookmark follows the new commit, and git sees it.
publish() { # workspace identity bookmark
  j "$2" -R "$1" bookmark set "$3" -r @-
  j "$2" -R "$1" git export
}
state() { j "$2" -R "$1" --ignore-working-copy log --no-graph -r @- -T "$STATE"; }
head_of() { j "$1" -R "$WT/$1" --ignore-working-copy log --no-graph -r @- -T 'commit_id'; }
clean() { eq "$(j "$1" -R "$WT/$1" log --no-graph -r @ -T "$PRE" | line 1)" true "$1 clean"; }

# §conflicts "How, with jj": the two rollbacks. Never applied to a commit a bookmark points at.
undo_pick() { # workspace identity state
  [ -z "$(echo "$3" | line 4)" ] || { echo "bookmarked: left as it is"; return 4; }
  j "$2" -R "$1" abandon @-
}
undo_merge() { # workspace identity state old-parent
  [ -z "$(echo "$3" | line 4)" ] || { echo "bookmarked: left as it is"; return 4; }
  j "$2" -R "$1" rebase -r @ --onto "commit_id($4)"
  j "$2" -R "$1" abandon "commit_id($(echo "$3" | line 1))"
}

# §ops "Cherry-pick". PHASE stops it early, to stand in for a timeout.
cherry_pick() { # id revset
  local id=$1 x=$2 w=$WT/$1
  clean "$id"
  local src; src=$(j "$id" -R "$w" --ignore-working-copy log --no-graph -r "$x" -T 'commit_id ++ " " ++ author.email() ++ "\n"')
  [ "$(echo "$src" | grep -c .)" = 1 ] || { echo "unknown commit"; return 2; }
  j "$id" -R "$w" --config "$DUP" duplicate "commit_id(${src%% *})" --insert-before @
  [ "${PHASE:-}" = mutated ] && return 0
  local st; st=$(state "$w" "$id")
  if [ "$(echo "$st" | line 2)" = true ] || [ "$(echo "$st" | line 3)" = true ]; then
    undo_pick "$w" "$id" "$st"
    echo "conflict: $(echo "$st" | sed -n '5,$p' | tr '\n' ' ')"; return 1
  fi
  publish "$w" "$id" "agents/$id"
  echo "${src#* }"
}

# §ops "Merge". $4 is the branch to move on success (none for integration, which moves its own).
merge() { # workspace identity branch [bookmark-to-publish]
  local w=$1 who=$2 br=$3
  local pre; pre=$(j "$who" -R "$w" log --no-graph -r @ -T "$PRE")
  eq "$(echo "$pre" | line 1)" true "clean before merge"
  local p; p=$(echo "$pre" | line 2)
  [ -n "$(j "$who" -R "$w" --ignore-working-copy log --no-graph -r "present(bookmarks(exact:\"$br\"))" -T 'commit_id ++ "\n"')" ] || { echo missing; return 3; }
  [ -n "$(j "$who" -R "$w" --ignore-working-copy log --no-graph -r "\"$br\" ~ ::@-" -T '"x\n"')" ] || { echo up-to-date; return 0; }
  j "$who" -R "$w" new --no-edit "--message=Merge $br" --insert-after @- --insert-after "\"$br\"" --insert-before @
  [ "${PHASE:-}" = mutated ] && return 0
  local st; st=$(state "$w" "$who")
  if [ "$(echo "$st" | line 2)" = true ]; then
    [ "${PHASE:-}" = half-undone ] && { j "$who" -R "$w" rebase -r @ --onto "commit_id($p)"; return 0; }
    undo_merge "$w" "$who" "$st" "$p"
    echo "conflict: $(echo "$st" | sed -n '5,$p' | tr '\n' ' ')"; return 1
  fi
  [ -z "${4:-}" ] || { j "$who" -R "$w" bookmark set "$4" -r @-; [ "${PHASE:-}" = bookmarked ] && return 0; j "$who" -R "$w" git export; }
  echo merged
}

# §conflicts "Timeouts": what runs after any command of an operation timed out.
recover() { # workspace identity bookmark old-parent pick|merge
  local w=$1 who=$2 bm=$3 p=$4 op=$5
  j "$who" -R "$w" workspace update-stale
  local st; st=$(j "$who" -R "$w" log --no-graph -r @- -T "$STATE")
  local head; head=$(echo "$st" | line 1)
  if [ "$head" = "$p" ]; then
    j "$who" -R "$w" git export; echo "nothing landed"; return 0
  fi
  if [ "$(echo "$st" | line 2)" = true ] || { [ "$op" = pick ] && [ "$(echo "$st" | line 3)" = true ]; }; then
    if [ "$op" = pick ]; then undo_pick "$w" "$who" "$st"; else undo_merge "$w" "$who" "$st" "$p"; fi
    j "$who" -R "$w" git export; echo "restored"; return 0
  fi
  publish "$w" "$who" "$bm"; echo "kept $head"
}

# ============================================================== hidden layout
WS=$T/proj; R=$WS/.crew/jj; WT=$WS/.crew/worktrees
git init -q -b main "$WS"; cd "$WS"
printf 'hello\ntwo\n' > a.txt; echo world > b.txt
git add -A; ugit commit -q -m first
head_before=$(git rev-parse HEAD); index_before=$(git ls-files -s | sha1sum)
mkdir -p "$WT"; printf '*\n' > .crew/.gitignore            # keeps .crew out of git status and of jj snapshots

prepare "$WS/.git"
eq "$(cat "$R/.jj/repo/store/git_target")" "$WS/.git" "git_target"
# base HEAD: the file names the branch, jj resolves the bookmark
eq "$(cat .git/HEAD)" "ref: refs/heads/main" "HEAD file"
BASE=$(bookmark main)
eq "$BASE" "$head_before" "base"

for id in a b d; do ensure_workspace "$id" "commit_id($BASE)"; done
[ -f "$WT/a/a.txt" ] || fail "workspace not populated"
eq "$(git rev-parse refs/heads/agents/a)" "$BASE" "exported agent branch"
( cd "$WT/a" && ! git rev-parse --show-toplevel >/dev/null 2>&1 ) || fail "git guard does not stop discovery"

# --- commit
printf 'hello\nA\n' > "$WT/a/a.txt"; echo n > "$WT/a/n.txt"
info=$(commit a "-a: change a.txt")
A1=$(echo "$info" | line 1)
eq "$(echo "$info" | line 3)" "-a: change a.txt" "subject"
eq "$(echo "$info" | line 4)" "2" "files changed"
eq "$(echo "$info" | line 5)" "exported" "export state"
eq "$(git rev-parse refs/heads/agents/a)" "$A1" "git sees the commit"
eq "$(git log -1 --format='%an <%ae>|%cn' agents/a)" "a <a@crew.local>|a" "identity"
git cat-file -p agents/a | grep -q '^change-id ' || fail "no change-id header"
commit a "again" >/dev/null && fail "empty commit was made" || true
! git ls-tree -r --name-only agents/a | grep -q '^\.git$\|^\.jj' || fail "guard or .jj committed"

# --- status and diff
echo more >> "$WT/a/b.txt"
out=$(j a -R "$WT/a" --ignore-working-copy log --no-graph -r @- -T 'bookmarks ++ "\n" ++ commit_id.short(8) ++ " " ++ description.first_line() ++ "\n"')
echo "$out" | grep -q '^agents/a$' || fail "status header"
( cd "$WT/a" && eq "$(j a -R "$WT/a" diff --summary)" "M b.txt" "diff --summary" )
out=$(j a -R "$WT/a" diff --git)
echo "$out" | grep -q '^+more$' || fail "diff --git"
out=$(j a -R "$WT/a" diff --stat --from '"main"' --to @)
echo "$out" | grep -q '3 files changed' || fail "diff --stat"
out=$(j a -R "$WT/a" diff --git --from "commit_id($BASE)" --to @)
echo "$out" | grep -q '^diff --git a/n.txt b/n.txt$' || fail "diff --from"
commit a "a: more" >/dev/null

# other agents: a clean commit and a conflicting one
echo "b new" > "$WT/b/bnew.txt"; B1=$(commit b "b: add bnew" | line 1)
printf 'hello\nD\n' > "$WT/d/a.txt"; D1=$(commit d $'d: conflicting\n\nbody' | line 1)

# --- cherry-pick
eq "$(cherry_pick a "commit_id(${B1:0:10})")" "b@crew.local" "pick source author"
[ -f "$WT/a/bnew.txt" ] || fail "pick did not land"
out=$(j a -R "$WT/a" --ignore-working-copy log --no-graph -r @- -T description)
echo "$out" | grep -q "^(cherry picked from commit $B1)$" || fail "trailer"
eq "$(git log -1 --format='%an|%cn' agents/a)" "b|a" "pick keeps author"
before=$(head_of a)
out=$(cherry_pick a "commit_id($D1)") && fail "conflict not reported" || true
eq "$out" "conflict: a.txt " "conflicted files"
eq "$(head_of a)" "$before" "pick rollback: head"
eq "$(cat "$WT/a/a.txt")" "$(printf 'hello\nA')" "pick rollback: file"
clean a
eq "$(bookmark agents/a)" "$before" "pick rollback: bookmark"
eq "$(git rev-parse refs/heads/agents/a)" "$before" "pick rollback: branch"
out=$(cherry_pick a "commit_id($B1)") && fail "empty pick kept" || true     # already applied: empty, rolled back
eq "$(head_of a)" "$before" "empty pick rollback"
cherry_pick a 'commit_id(deadbeefdeadbeef)' >/dev/null && fail "unknown commit accepted" || true
CH=$(j a -R "$WT/a" --ignore-working-copy log --no-graph -r "commit_id($D1)" -T change_id)
eq "$(j a -R "$WT/a" --ignore-working-copy log --no-graph -r "change_id(${CH:0:12})" -T commit_id)" "$D1" "change id lookup"

# --- merge
eq "$(merge "$WT/b" b agents/a agents/b)" merged "merge"
eq "$(git log -1 --format='%s|%an' agents/b)" "Merge agents/a|b" "merge commit"
eq "$(git rev-list --parents -1 agents/b | wc -w)" 3 "two parents"
eq "$(merge "$WT/b" b agents/a agents/b)" up-to-date "second merge"
before=$(head_of b)
out=$(merge "$WT/b" b agents/d agents/b) && fail "merge conflict not reported" || true
eq "$out" "conflict: a.txt " "merge conflicted files"
eq "$(head_of b)" "$before" "merge rollback: head"; clean b
eq "$(cat "$WT/b/a.txt")" "$(printf 'hello\nA')" "merge rollback: file"
eq "$(bookmark agents/b)" "$before" "merge rollback: bookmark"
merge "$WT/b" b agents/nobody >/dev/null && fail "missing branch merged" || true

# --- timeouts (§conflicts): an operation cut short at each phase, then `recover`
ensure_workspace t1 "commit_id($BASE)"; ensure_workspace t2 "commit_id($BASE)"
echo t2 > "$WT/t2/t2.txt"; commit t2 "t2 work" >/dev/null
p=$(head_of t1)
#  nothing landed
eq "$(recover "$WT/t1" t1 agents/t1 "$p" merge)" "nothing landed" "recover: nothing"
#  the merge landed clean, the bookmark did not move yet: kept and published
PHASE=mutated merge "$WT/t1" t1 agents/t2 agents/t1
m=$(head_of t1); [ "$m" != "$p" ] || fail "merge did not land"
eq "$(recover "$WT/t1" t1 agents/t1 "$p" merge)" "kept $m" "recover: after the mutation"
eq "$(bookmark agents/t1)" "$m" "recover: bookmark follows a kept result"
eq "$(git rev-parse refs/heads/agents/t1)" "$m" "recover: kept result exported"
#  cut after `bookmark set`, before `git export` (the reviewer's case): kept, never rolled back
echo t2b > "$WT/t2/t2b.txt"; commit t2 "t2 more" >/dev/null
p=$(head_of t1)
PHASE=bookmarked merge "$WT/t1" t1 agents/t2 agents/t1
m=$(head_of t1); eq "$(bookmark agents/t1)" "$m" "bookmark moved"; eq "$(git rev-parse refs/heads/agents/t1)" "$p" "not exported yet"
eq "$(recover "$WT/t1" t1 agents/t1 "$p" merge)" "kept $m" "recover: after the bookmark moved"
eq "$(bookmark agents/t1)" "$m" "bookmark survives recovery"
eq "$(git rev-parse refs/heads/agents/t1)" "$m" "ref exported by recovery"
#  cut after the export: the same
eq "$(recover "$WT/t1" t1 agents/t1 "$p" merge)" "kept $m" "recover: after the export"
eq "$(git rev-parse refs/heads/agents/t1)" "$m" "ref after a second recovery"
#  a conflicted merge landed: rolled back; bookmark and ref stay on the old parent
printf 'hello\nT\n' > "$WT/t1/a.txt"; commit t1 "t1: change a.txt" >/dev/null
p=$(head_of t1)
PHASE=mutated merge "$WT/t1" t1 agents/d agents/t1
eq "$(recover "$WT/t1" t1 agents/t1 "$p" merge)" "restored" "recover: conflicted merge"
eq "$(head_of t1)" "$p" "head restored"; clean t1
eq "$(bookmark agents/t1)" "$p" "bookmark untouched by rollback"; eq "$(git rev-parse refs/heads/agents/t1)" "$p" "ref untouched by rollback"
#  cut in the middle of the rollback (rebase done, abandon not)
PHASE=half-undone merge "$WT/t1" t1 agents/d agents/t1
eq "$(recover "$WT/t1" t1 agents/t1 "$p" merge)" "nothing landed" "recover: half-undone merge"
eq "$(bookmark agents/t1)" "$p" "bookmark after a half-undone merge"
#  a conflicted pick landed
PHASE=mutated cherry_pick t1 "commit_id($D1)"
eq "$(recover "$WT/t1" t1 agents/t1 "$p" pick)" "restored" "recover: conflicted pick"
eq "$(head_of t1)" "$p" "pick head restored"; eq "$(bookmark agents/t1)" "$p" "pick bookmark"
#  a clean pick landed: kept and published
PHASE=mutated cherry_pick t1 "commit_id($B1)"
k=$(head_of t1)
eq "$(recover "$WT/t1" t1 agents/t1 "$p" pick)" "kept $k" "recover: clean pick"
eq "$(git rev-parse refs/heads/agents/t1)" "$k" "pick exported by recovery"
#  the guard: a commit that carries a bookmark is never abandoned
out=$(undo_pick "$WT/t1" t1 "$(state "$WT/t1" t1)") && fail "bookmarked commit abandoned" || true
eq "$out" "bookmarked: left as it is" "rollback guard"; eq "$(bookmark agents/t1)" "$k" "guarded bookmark"

# --- integration
j crew -R "$R" --ignore-working-copy bookmark create agents/integration -r "commit_id($BASE)"
j crew -R "$R" workspace add --name _integration --sparse-patterns empty -r '"agents/integration"' "$WT/_integration"
j crew -R "$WT/_integration" sparse reset
I=$WT/_integration; merged=""; conflicted=""
for id in a b d ghost; do
  out=$(merge "$I" crew "agents/$id") || true
  case "$out" in
    merged) j crew -R "$I" bookmark set agents/integration -r @-; merged="$merged $id" ;;
    conflict*) conflicted="$conflicted $id" ;;
  esac
done
j crew -R "$I" git export
eq "$merged" " a b" "integration merged"; eq "$conflicted" " d" "integration conflicts"
[ -f "$I/bnew.txt" ] && [ -f "$I/n.txt" ] || fail "integration tree"
eq "$(git log -1 --format='%an' agents/integration)" "crew" "integration identity"

# --- the bookmark never moves sideways
j a -R "$WT/a" bookmark set agents/a -r '"main"' 2>/dev/null && fail "sideways move allowed" || true

# --- a stale workspace is an error, not an automatic update. Rewriting a commit
# from another workspace makes every workspace that sits on it stale.
ensure_workspace s1 "commit_id($BASE)"; ensure_workspace s2 "commit_id($BASE)"
echo s1 > "$WT/s1/s1.txt"; commit s1 "s1 work" >/dev/null
echo inj > "$WT/s2/inj.txt"
j s2 -R "$WT/s2" squash --from @ --into '"agents/s1"' -u
out=$(j s1 -R "$WT/s1" status 2>&1) && fail "stale workspace accepted" || true
echo "$out" | grep -q "working copy is stale" || fail "stale not reported"
j s1 -R "$WT/s1" workspace update-stale
j s1 -R "$WT/s1" status >/dev/null
[ -f "$WT/s1/inj.txt" ] || fail "update-stale did not bring the rewritten commit"
# --- a workspace directory removed by hand
rm -rf "$WT/d"
j crew -R "$R" --ignore-working-copy workspace forget d
ensure_workspace d "commit_id($BASE)"
[ -f "$WT/d/a.txt" ] || fail "re-added workspace"

# --- the store works with the read-only top levels the sandbox mounts (§sandbox):
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
eq "$(merge "$WT/e" e agents/a agents/e)" merged "merge with read-only store"
out=$(merge "$WT/e" e agents/d agents/e) && fail "conflict with read-only store" || true
eq "$out" "conflict: a.txt " "rollback with read-only store"; clean e
chmod u+w "$R/.jj/repo" "$R/.jj/repo/store" "$WT/b/.jj" "$WT/e/.jj" .git

# --- the user's checkout was never touched
eq "$(git rev-parse HEAD)" "$head_before" "user HEAD"
eq "$(cat .git/HEAD)" "ref: refs/heads/main" "user HEAD ref"
eq "$(git ls-files -s | sha1sum)" "$index_before" "user index"
eq "$(git status --porcelain)" "" "user working tree"
[ ! -e "$WS/.jj" ] || fail ".jj in the project root"
find "$WS" -name '*.lock' | grep -q . && fail "lock files left" || true

# ============================================ hidden layout, linked git worktree
# The project is a linked worktree of another checkout: `.git` is a file, HEAD
# lives in the admin directory, objects and refs in the common directory.
git init -q -b main "$T/main"; cd "$T/main"; echo hello > a.txt; git add -A; ugit commit -q -m first
git worktree add -q "$T/linked" -b feature; cd "$T/linked"
echo feat > feat.txt; git add feat.txt; ugit commit -q -m "feature work"
ADMIN=$(sed -n 's/^gitdir: //p' .git)
COMMON=$(cd "$ADMIN" && cd "$(cat commondir)" && pwd)
eq "$COMMON" "$T/main/.git" "common directory"
eq "$(cat "$ADMIN/HEAD")" "ref: refs/heads/feature" "linked HEAD"
eq "$(cat "$COMMON/HEAD")" "ref: refs/heads/main" "main HEAD"
WS=$T/linked; R=$WS/.crew/jj; WT=$WS/.crew/worktrees
mkdir -p "$WT"; printf '*\n' > .crew/.gitignore
prepare "$COMMON"
eq "$(j crew -R "$R" --ignore-working-copy git root)" "$COMMON" "git root is the common directory"
LBASE=$(bookmark feature)                                   # the linked HEAD, not main's
eq "$LBASE" "$(git rev-parse feature)" "linked base"; [ "$LBASE" != "$(git rev-parse main)" ] || fail "linked base is main"
ensure_workspace a "commit_id($LBASE)"
[ -f "$WT/a/feat.txt" ] || fail "linked workspace starts from the linked HEAD"
echo w > "$WT/a/w.txt"; L1=$(commit a "a in linked" | line 1)
eq "$(git -C "$T/main" rev-parse refs/heads/agents/a)" "$L1" "branch in the common repository"
eq "$(git status --porcelain)" "" "linked checkout untouched"; eq "$(cat "$ADMIN/HEAD")" "ref: refs/heads/feature" "linked HEAD untouched"
eq "$(git -C "$T/main" status --porcelain)" "" "main checkout untouched"; eq "$(cat "$COMMON/HEAD")" "ref: refs/heads/main" "main HEAD untouched"

# ================================================= moving a workspace git -> jj
# §decisions D2: a workspace the git backend provisioned, moved by hand.
MS=$T/migrate; git init -q -b main "$MS"; cd "$MS"; echo hello > a.txt; git add -A; ugit commit -q -m first
mkdir -p .crew/worktrees; printf '*\n' > .crew/.gitignore
git worktree add -q .crew/worktrees/a -b agents/a main
echo "a's work" > .crew/worktrees/a/wa.txt; git -C .crew/worktrees/a add -A
git -C .crew/worktrees/a -c user.name=a -c user.email=a@crew.local commit -q -m "a: committed under git"
G1=$(git rev-parse agents/a)
git worktree remove .crew/worktrees/a; git worktree prune
WS=$MS; R=$WS/.crew/jj; WT=$WS/.crew/worktrees
prepare "$WS/.git"
eq "$(bookmark agents/a)" "$G1" "imported agent branch"
ensure_workspace a "commit_id($(bookmark main))"
[ -f "$WT/a/wa.txt" ] || fail "migrated workspace does not start from its branch"
echo more > "$WT/a/more.txt"; commit a "a: continued under jj" >/dev/null
eq "$(git rev-parse agents/a~1)" "$G1" "jj continues the git branch"

# ================================================= moving a workspace jj -> git
# §decisions D2 "Back to git": the team is stopped and a jj workspace still has
# uncommitted work. By hand, with plain jj (no pins but the ones the steps give).
hand() { JJ_USER=u JJ_EMAIL=u@example.com "$JJ" --no-pager --color never --quiet "$@"; }
BIG='snapshot.max-new-file-size="1TiB"'
J1=$(git rev-parse agents/a)
echo dirty > "$WT/a/dirty.txt"; head -c 1500000 /dev/zero > "$WT/a/big.bin"
out=$(hand -R "$WT/a" --config "$BIG" status)
echo "$out" | grep -q 'dirty.txt' || fail "status does not list the uncommitted file"
hand -R "$WT/a" --config "$BIG" commit -m "a: saved by hand"
SAVED=$(hand -R "$WT/a" log --no-graph -r @- -T 'commit_id ++ "\n"')
[ "$SAVED" != "$J1" ] || fail "nothing was saved"
# the hazard the steps exist for: `jj commit` alone moves neither the bookmark nor the git branch
eq "$(bookmark agents/a)" "$J1" "bare jj commit leaves the bookmark behind"
eq "$(git rev-parse agents/a)" "$J1" "bare jj commit leaves the git branch behind"
# the steps: move the bookmark, export, verify
hand -R "$WT/a" bookmark set agents/a -r @-
hand -R "$WT/a" git export
eq "$(git rev-parse agents/a)" "$(hand -R "$WT/a" log --no-graph -r @- -T 'commit_id ++ "\n"')" "verify: the exported branch is the saved commit"
out=$(hand -R "$WT/a" --config "$BIG" status)
echo "$out" | grep -q 'The working copy has no changes' || fail "verify: the workspace is clean"
# only now: remove jj's state; the git backend adds its worktree on the branch
rm -rf "$R" "$WT/a"
git worktree add -q .crew/worktrees/a agents/a
eq "$(git -C .crew/worktrees/a rev-parse HEAD)" "$SAVED" "git worktree is at the saved commit"
[ -f .crew/worktrees/a/dirty.txt ] && [ -f .crew/worktrees/a/more.txt ] && [ -f .crew/worktrees/a/wa.txt ] || fail "saved work missing from the git worktree"
eq "$(wc -c < .crew/worktrees/a/big.bin)" 1500000 "the large file was saved"
eq "$(git -C .crew/worktrees/a status --porcelain)" "" "git worktree is clean"
# jj's keep refs stay behind in git; this removes them
git for-each-ref --format='delete %(refname)' refs/jj/ | git update-ref --stdin
eq "$(git for-each-ref refs/jj/ | wc -l)" 0 "keep refs removed"
git fsck --connectivity-only >/dev/null 2>&1 || fail "repository damaged"
eq "$(git rev-parse agents/a)" "$SAVED" "branch after cleanup"

# ================================================================ native layout
NS=$T/native; mkdir -p "$NS"; cd "$NS"
git init -q -b main .; echo hi > a.txt; git add -A; ugit commit -q -m first
JJ_USER=u JJ_EMAIL=u@example.com "$JJ" --quiet git init --colocate
mkdir -p .crew/worktrees; printf '*\n' > .crew/.gitignore; echo dirty > dirty.txt
R=$NS/.crew/jj; WT=$NS/.crew/worktrees
# bootstrap: the user's own config applies to the snapshot of their working copy
( unset JJ_CONFIG; JJ_USER=crew JJ_EMAIL=crew@crew.local "$JJ" --no-pager --color never --quiet \
    --config signing.behavior=drop --config signing.backend=none --config fsmonitor.backend=none --config 'ui.editor="true"' \
    -R "$NS" workspace add --name crew --sparse-patterns empty -r 'root()' "$R" )
j crew -R "$R" --ignore-working-copy workspace forget crew
NBASE=$(j crew -R "$NS" --ignore-working-copy log --no-graph -r @- -T 'commit_id ++ "\n"')
eq "$NBASE" "$(git rev-parse main)" "native base"
eq "$(j crew -R "$R" --ignore-working-copy git root)" "$NS/.git" "native git root"
ensure_workspace a "commit_id($NBASE)"
eq "$(git rev-parse refs/heads/agents/a)" "$NBASE" "native export"
JJ_USER=u JJ_EMAIL=u@example.com "$JJ" --no-pager --color never status | grep -q '\.crew' && fail "user's jj sees .crew" || true

echo "PASS: every argv of notes/jj-design.md ran on $ver"
