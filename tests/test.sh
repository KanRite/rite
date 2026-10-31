#!/usr/bin/env bash
#
# End-to-end spec for the rite CLI. Run: bash tests/test.sh
#
# Builds the binary with cargo, then drives it against throwaway git repos.
# Unit tests for the pure logic (slugify, frontmatter, templates) live in the
# Rust sources and run under `cargo test`.
#
# RITE_NO_INPUT=1 keeps the run hermetic: the suite covers the non-interactive
# paths, and without it a prompt would block when the suite is run from a
# terminal.
#
set -uo pipefail

export RITE_NO_INPUT=1
# Keep the suite away from the real registry and theme: `rite register` writes
# to the config directory, and a test must never touch the user's. State and
# data too: every run moves an agile-md folder it finds to kanrite (ticket
# 310), which must only ever happen to the suite's own.
# Temporary folders by their real path: on macOS $TMPDIR is under /var, a link
# to /private/var, and rite prints the real path, so a test that looks for the
# one it made would never find it (ticket 345). Harmless where there's no link.
TMPDIR="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
export TMPDIR
xdg="$(mktemp -d)"
XDG_CONFIG_HOME="${xdg}/config"
XDG_STATE_HOME="${xdg}/state"
XDG_DATA_HOME="${xdg}/data"
export XDG_CONFIG_HOME XDG_STATE_HOME XDG_DATA_HOME

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ -n "${RITE_BIN:-}" ]; then
  AMD="${RITE_BIN}"
else
  echo "building rite..."
  (cd "${ROOT}" && cargo build --quiet) || exit 1
  AMD="${ROOT}/target/debug/rite"
fi
[ -x "${AMD}" ] || { echo "no rite binary at ${AMD}"; exit 1; }

pass=0
fail=0
assert() { # description  command...
  local d="$1"; shift
  if "$@" >/dev/null 2>&1; then echo "  ok   - ${d}"; pass=$((pass + 1))
  else echo "  FAIL - ${d}"; fail=$((fail + 1)); fi
}

tmp="$(mktemp -d)"
nongit="$(mktemp -d)"
trap 'rm -rf "${tmp}" "${nongit}"' EXIT
cd "${tmp}" || exit 1
git init -q; git config user.email t@t.co; git config user.name t

echo "init:"
"${AMD}" init acme --local-only >/dev/null
assert "init creates backlog/todo/doing/done" \
  test -d tasks/backlog -a -d tasks/todo -a -d tasks/doing -a -d tasks/done
assert "init writes the repository README, not inside tasks/" \
  bash -c "test -f README.md -a ! -f tasks/README.md"
assert "init records the declared project in the repository, not inside tasks/" \
  bash -c "grep -q '^project = acme\$' .settings && test ! -f tasks/.settings"
assert "init records whoever ran it as the project's first user (ticket 090)" \
  grep -qx 'users = t <t@t.co>' .settings
assert "a second init with the same project is a silent no-op" \
  "${AMD}" init acme --local-only
assert "init with a different project is refused, naming both" \
  bash -c "'${AMD}' init other --local-only 2>&1 | grep -q \"already 'acme'\" && grep -q '^project = acme\$' .settings"
assert "an invalid project name is rejected before anything is created" \
  bash -c "'${AMD}' init 'not valid' --local-only 2>&1 | grep -q 'letters, numbers, hyphens, periods, colons and underscores'"

periods_and_underscores="$(mktemp -d)"
( cd "${periods_and_underscores}" && git init -q && git config user.email t@t.co && git config user.name t )
assert "periods and underscores are valid in a project name" \
  bash -c "cd '${periods_and_underscores}' && '${AMD}' init 'a.valid_name' --local-only >/dev/null && grep -q '^project = a.valid_name\$' .settings"
# init --local-only registers the board it creates locally — undo it here
# so it doesn't leave a stray local-registry entry ahead of the dedicated
# "registration:" tests below, which check that file's exact contents.
"${AMD}" deregister "$(basename "${periods_and_underscores}")" >/dev/null 2>&1

echo "init: a dedicated tickets repository, found locally:"
# Deliberately a plain, non-git directory: `rite init <project>` must not
# require being run from inside an existing git repository — it creates or
# finds `<project>-tickets`/`<project>-documents` as children of wherever
# it's run from. Both pre-created here (empty, local-only git repos, no
# 'origin') so `init` finds both and never calls out to `gh` at all —
# hitting the real network for a nonexistent "widgets" owner would just
# fail, and this test has nothing to do with `gh repo create` itself.
plain_dir="$(mktemp -d)"
( cd "${plain_dir}" && git init -q widgets-tickets \
  && cd widgets-tickets && git config user.email t@t.co && git config user.name t )
( cd "${plain_dir}" && git init -q widgets-documents \
  && cd widgets-documents && git config user.email t@t.co && git config user.name t )
assert "rite init <project> works from a plain, non-git directory" \
  bash -c "cd '${plain_dir}' && '${AMD}' init widgets >/dev/null && test -d '${plain_dir}/widgets-tickets/tasks/backlog'"
assert "the found-locally tickets repo's own board records the project" \
  bash -c "grep -q '^project = widgets\$' '${plain_dir}/widgets-tickets/.settings'"
assert "the found-locally documents repo is scaffolded, not a board" \
  bash -c "test -d '${plain_dir}/widgets-documents/adr/adr' -a -d '${plain_dir}/widgets-documents/adr/rfc' \
    && test -d '${plain_dir}/widgets-documents/docs' -a -d '${plain_dir}/widgets-documents/ideas' \
    && test ! -d '${plain_dir}/widgets-documents/tasks' \
    && test ! -e '${plain_dir}/widgets-documents/.settings'"
assert "init records you as the first user of a found-locally project" \
  grep -qx 'users = t <t@t.co>' "${plain_dir}/widgets-tickets/.settings"
assert "...in a commit of its own, since the repository already existed" \
  bash -c "[ \"\$(git -C '${plain_dir}/widgets-tickets' log -1 --format=%s)\" = 'init: add t <t@t.co> to users' ]"
assert "running init again doesn't list you twice or commit again" \
  bash -c "cd '${plain_dir}' && before=\"\$(git -C widgets-tickets rev-parse HEAD)\" \
    && '${AMD}' init widgets >/dev/null 2>&1 \
    && [ \"\$(git -C widgets-tickets rev-parse HEAD)\" = \"\${before}\" ] \
    && [ \"\$(grep -c '^users = ' widgets-tickets/.settings)\" -eq 1 ]"
# Deregistering both proves init registered them and cleans up the registry
# ahead of the dedicated "registration:" tests below, which check that
# file's exact contents.
assert "rite init registered the tickets repo" \
  bash -c "'${AMD}' deregister widgets-tickets | grep -q 'deregistered widgets-tickets'"
assert "rite init registered the documents repo" \
  bash -c "'${AMD}' deregister widgets-documents | grep -q 'deregistered widgets-documents'"
# Ticket 263: rite init brings an archived project back. With its clones
# here (no origin, so no GitHub): archived turned off and both registered.
( cd "${plain_dir}/widgets-tickets" && printf 'archived = on\n' >> .settings \
  && git add .settings && git commit -q -m 'settings: archived on' )
assert "rite init brings back an archived project whose clones are here" \
  bash -c "cd '${plain_dir}' && out=\$('${AMD}' init widgets 2>&1) && echo \"\$out\" | grep -q '^unarchived widgets\$' \
    && ! grep -q archived widgets-tickets/.settings \
    && '${AMD}' deregister widgets-tickets | grep -q deregistered \
    && '${AMD}' deregister widgets-documents | grep -q deregistered"
# With nothing cloned: a fake gh says both repositories exist and are
# archived on GitHub; init unarchives them and clones, never creates.
bb="$(mktemp -d)"
mkdir -p "${bb}/bin" "${bb}/remote" "${bb}/here"
for kind in tickets documents; do
  git init -q -b main "${bb}/seed-${kind}" && ( cd "${bb}/seed-${kind}" \
    && git config user.email t@t.co && git config user.name t && git config commit.gpgsign false \
    && echo seed > README.md && git add -A && git commit -q -m seed \
    && git clone -q --bare . "${bb}/remote/gizmo-${kind}.git" )
done
cat > "${bb}/bin/gh" <<'GH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_LOG:?}"
case "$1 $2" in
  "repo view") echo true ;;
  "repo clone") git clone -q "${GH_REMOTE:?}/${3#*/}.git" "$4" ;;
  "repo create") echo "should not create" >&2; exit 1 ;;
esac
GH
chmod +x "${bb}/bin/gh"
assert "rite init clones archived repositories from GitHub, unarchiving them, and never creates" \
  bash -c "cd '${bb}/here' && env PATH='${bb}/bin:${PATH}' GH_LOG='${bb}/gh.log' GH_REMOTE='${bb}/remote' \
      XDG_CONFIG_HOME='${bb}/config' GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t.co GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t.co \
      '${AMD}' init gizmo >/dev/null 2>&1; \
    grep -qx 'repo unarchive gizmo/gizmo-tickets --yes' '${bb}/gh.log' \
    && grep -qx 'repo unarchive gizmo/gizmo-documents --yes' '${bb}/gh.log' \
    && ! grep -q 'repo create' '${bb}/gh.log' \
    && test -f gizmo-tickets/README.md -a -d gizmo-tickets/tasks/backlog \
    && grep -q gizmo-tickets '${bb}/config/kanrite/repos'"
rm -rf "${bb}"

# Ticket 277: repositories init creates say LF for every text file, so a
# Windows clone has the same bytes as anyone's. A fake gh that finds nothing,
# creates a bare repository beside us, and refuses protection as the free
# plan does.
nw="$(mktemp -d)"
mkdir -p "${nw}/bin" "${nw}/remote" "${nw}/here"
cat > "${nw}/bin/gh" <<'GH'
#!/usr/bin/env bash
case "$1 $2" in
  "repo view") exit 1 ;;
  "repo create") name="${3#*/}"
    git init -q --bare -b main "${GH_REMOTE:?}/${name}.git" \
      && git clone -q "${GH_REMOTE}/${name}.git" "${name}" 2>/dev/null ;;
  "api "*) cat >/dev/null; echo "HTTP 403: Upgrade to GitHub Pro" >&2; exit 1 ;;
esac
GH
chmod +x "${nw}/bin/gh"
assert "rite init gives the repositories it creates a .gitattributes keeping text LF" \
  bash -c "cd '${nw}/here' && env PATH='${nw}/bin:${PATH}' GH_REMOTE='${nw}/remote' \
      XDG_CONFIG_HOME='${nw}/config' GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t.co GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t.co \
      '${AMD}' init sprocket >/dev/null 2>&1; \
    for kind in tickets documents; do \
      git -C sprocket-\${kind} ls-files --error-unmatch .gitattributes >/dev/null 2>&1 \
        && grep -qx '\* text=auto eol=lf' sprocket-\${kind}/.gitattributes || exit 1; \
    done"
assert "...and leaves a repository that was already there without one" \
  bash -c "test ! -e '${plain_dir}/widgets-tickets/.gitattributes' -a ! -e '${plain_dir}/widgets-documents/.gitattributes'"
rm -rf "${nw}"

echo "init: an existing project made before ticket 090 gets its first user:"
# A project whose remote has no users yet, cloned locally the way its
# creator already has it; re-running init adds them and pushes.
backfill="$(mktemp -d)"
maker="GIT_CONFIG_GLOBAL=${backfill}/gitconfig GIT_CONFIG_NOSYSTEM=1 XDG_CONFIG_HOME=${backfill}/config"
printf '[user]\n\tname = maker\n\temail = maker@t.co\n[commit]\n\tgpgsign = false\n' >"${backfill}/gitconfig"
(
  set -e
  export GIT_CONFIG_GLOBAL="${backfill}/gitconfig" GIT_CONFIG_NOSYSTEM=1 XDG_CONFIG_HOME="${backfill}/config"
  cd "${backfill}" && git init -q -b main seed && cd seed
  "${AMD}" init old --local-only >/dev/null
  sed -i '/^users = /d' .settings
  git add -A && git commit -q -m "seed for ticket 090"
  git init -q --bare -b main "${backfill}/origin/old-tickets.git"
  git push -q "${backfill}/origin/old-tickets.git" HEAD:main
  mkdir -p "${backfill}/work" && cd "${backfill}/work"
  git clone -q "${backfill}/origin/old-tickets.git" old-tickets
  git init -q old-documents
)
assert "re-running init on it adds you and says so" \
  bash -c "cd '${backfill}/work' && out=\"\$(env ${maker} '${AMD}' init old 2>/dev/null)\" \
    && grep -q 'added you (maker <maker@t.co>)' <<<\"\${out}\""
assert "...pushed, so the remote lists you" \
  bash -c "git --git-dir='${backfill}/origin/old-tickets.git' show main:.settings | grep -qx 'users = maker <maker@t.co>'"
nobody="$(mktemp -d)"
( cd "${nobody}" && git init -q )
assert "init with no git email still succeeds, and warns that you weren't added" \
  bash -c "cd '${nobody}' && env GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 XDG_CONFIG_HOME='${nobody}/config' \
    '${AMD}' init lonely --local-only 2>&1 >/dev/null | grep -q \"weren't added to lonely's users\" \
    && grep -qx 'project = lonely' .settings && ! grep -q '^users' .settings"
rm -rf "${backfill}" "${nobody}"

echo "init: catches up an existing clone before recording you (ticket 094):"
# A remote with no users; three clones of it made while it was like that.
# Then someone else joins (the remote's users line changes), so each clone
# is behind on the very line init writes.
catchup="$(mktemp -d)"
as_maker="GIT_CONFIG_GLOBAL=${catchup}/gitconfig GIT_CONFIG_NOSYSTEM=1 XDG_CONFIG_HOME=${catchup}/config"
printf '[user]\n\tname = maker\n\temail = maker@t.co\n[commit]\n\tgpgsign = false\n' >"${catchup}/gitconfig"
(
  set -e
  export GIT_CONFIG_GLOBAL="${catchup}/gitconfig" GIT_CONFIG_NOSYSTEM=1 XDG_CONFIG_HOME="${catchup}/config"
  cd "${catchup}" && git init -q -b main seed && cd seed
  "${AMD}" init late --local-only >/dev/null
  sed -i '/^users = /d' .settings
  git add -A && git commit -q -m "seed for ticket 094"
  git init -q --bare -b main "${catchup}/origin.git"
  git push -q "${catchup}/origin.git" HEAD:main
  for kind in behind dirty diverged; do
    mkdir -p "${catchup}/${kind}" && cd "${catchup}/${kind}"
    git clone -q "${catchup}/origin.git" late-tickets
    git init -q late-documents
  done
  cd "${catchup}/seed"
  printf 'users = other <other@t.co>\n' >>.settings
  git commit -q -am "join: other <other@t.co> (ticket 094 seed)"
  git push -q "${catchup}/origin.git" HEAD:main
  cd "${catchup}/dirty/late-tickets" && echo "half-done" >>README.md
  cd "${catchup}/diverged/late-tickets" && echo "mine" >>README.md \
    && git commit -q -am "local work (ticket 094 seed)"
)
remote_users() { git --git-dir="${catchup}/origin.git" show main:.settings | grep '^users = '; }
assert "a clean clone that's behind is fast-forwarded, then you're added and pushed" \
  bash -c "cd '${catchup}/behind' && out=\"\$(env ${as_maker} '${AMD}' init late 2>&1)\" \
    && grep -q 'caught up: 1 new change' <<<\"\${out}\" \
    && grep -q 'added you (maker <maker@t.co>)' <<<\"\${out}\" \
    && [ -z \"\$(git -C late-tickets rev-list @{u}..HEAD)\" ]"
assert "...so the remote lists both of you" \
  bash -c "[ \"\$(git --git-dir='${catchup}/origin.git' show main:.settings | grep '^users = ')\" \
    = 'users = other <other@t.co>, maker <maker@t.co>' ]"
before_users="$(remote_users)"
dirty_head="$(git -C "${catchup}/dirty/late-tickets" rev-parse HEAD)"
assert "a clone with uncommitted edits is left alone, with a warning" \
  bash -c "cd '${catchup}/dirty' && env ${as_maker} '${AMD}' init late 2>&1 >/dev/null \
    | grep -q \"has uncommitted changes — commit or stash them first, so you weren't added to late's users\""
assert "...untouched: same commit, the edit still there, nobody added" \
  bash -c "cd '${catchup}/dirty/late-tickets' && [ \"\$(git rev-parse HEAD)\" = '${dirty_head}' ] \
    && grep -q half-done README.md && ! grep -q '^users' .settings"
diverged_head="$(git -C "${catchup}/diverged/late-tickets" rev-parse HEAD)"
assert "a clone that has diverged is left alone, with how to fix it" \
  bash -c "cd '${catchup}/diverged' && env ${as_maker} '${AMD}' init late 2>&1 >/dev/null \
    | grep -q 'run \`git pull --rebase\` there'"
assert "...untouched, and the remote's users unchanged" \
  bash -c "[ \"\$(git -C '${catchup}/diverged/late-tickets' rev-parse HEAD)\" = '${diverged_head}' ] \
    && [ \"\$(git --git-dir='${catchup}/origin.git' show main:.settings | grep '^users = ')\" = '${before_users}' ]"
rm -rf "${catchup}"

echo "init: refuses to run inside an existing git repository:"
guard_test="$(mktemp -d)"
( cd "${guard_test}" && git init -q code_repo \
  && cd code_repo && git config user.email t@t.co && git config user.name t \
  && git commit -q --allow-empty -m seed )
assert "rite init <project> refuses to run inside an existing repository" \
  bash -c "cd '${guard_test}/code_repo' && '${AMD}' init gadgets 2>&1 | grep -q 'must not be run inside a git repository'"
assert "nothing was created by the refused attempt" \
  test ! -e "${guard_test}/gadgets-tickets" -a ! -e "${guard_test}/code_repo/gadgets-tickets" \
  -a ! -e "${guard_test}/gadgets-documents" -a ! -e "${guard_test}/code_repo/gadgets-documents"

echo "join:"
# `rite join` clones over plain git, so local bare repositories stand in for
# GitHub: RITE_GIT_BASE points it at "${origin}/", and the URL it builds is
# "${origin}/<owner>/<project>-tickets.git". Every simulated person gets
# their own global git config (their identity) and their own config
# directory (their registry), the way two machines would — and so none of
# this touches the suite's own registry, which "registration:" below checks
# exactly.
origin="$(mktemp -d)"
people="$(mktemp -d)"
person() { # name email -> prints the env assignments for that person
  local home="${people}/$1"
  mkdir -p "${home}/config" "${home}/work"
  printf '[user]\n\tname = %s\n\temail = %s\n[commit]\n\tgpgsign = false\n' "$1" "$2" >"${home}/gitconfig"
  echo "GIT_CONFIG_GLOBAL=${home}/gitconfig XDG_CONFIG_HOME=${home}/config RITE_GIT_BASE=${origin}/"
}
seed_bare() { # project kind -> a bare repo on main, seeded from the current directory
  git init -q --bare -b main "${origin}/$1/$1-$2.git"
  git push -q "${origin}/$1/$1-$2.git" HEAD:main
}
(
  set -e
  export GIT_CONFIG_GLOBAL="${people}/seed.gitconfig" XDG_CONFIG_HOME="${people}/seed-config"
  printf '[user]\n\tname = seed\n\temail = seed@t.co\n' >"${GIT_CONFIG_GLOBAL}"
  seed="$(mktemp -d)"
  cd "${seed}" && git init -q -b main acme-tickets && cd acme-tickets
  "${AMD}" init acme --local-only >/dev/null
  git add -A && git commit -q -m seed && seed_bare acme tickets
  cd "${seed}" && git init -q -b main acme-documents && cd acme-documents
  git commit -q --allow-empty -m seed && seed_bare acme documents
  # A project whose tickets repository has no board in it at all.
  cd "${seed}" && git init -q -b main blank && cd blank
  git commit -q --allow-empty -m seed && seed_bare blank tickets && seed_bare blank documents
  rm -rf "${seed}"
)
remote_settings() { git --git-dir="${origin}/acme/acme-tickets.git" show main:.settings; }

ada="$(person ada ada@t.co)"
assert "rite join clones both repositories side by side" \
  bash -c "cd '${people}/ada/work' && env ${ada} '${AMD}' join acme >/dev/null \
    && test -d acme-tickets/tasks/backlog && test -d acme-documents/.git"
assert "the joining user is added to the remote's users" \
  bash -c "$(declare -f remote_settings); origin='${origin}'; remote_settings | grep -q '^users = .*ada <ada@t.co>'"
assert "both repositories are registered for the joining user" \
  bash -c "cd '${people}/ada/work' && env ${ada} '${AMD}' deregister acme-tickets | grep -q 'deregistered acme-tickets' \
    && env ${ada} '${AMD}' deregister acme-documents | grep -q 'deregistered acme-documents'"
assert "joining again (fresh directory) doesn't list the user twice" \
  bash -c "mkdir -p '${people}/ada/again' && cd '${people}/ada/again' \
    && env ${ada} '${AMD}' join acme | grep -q 'already one of its users' \
    && [ \"\$(git --git-dir='${origin}/acme/acme-tickets.git' show main:.settings | grep -o 'ada@t.co' | wc -l)\" -eq 1 ]"
assert "join refuses when the tickets directory is already there, touching nothing" \
  bash -c "cd '${people}/ada/work' && before=\"\$(git -C acme-tickets rev-parse HEAD)\" \
    && ! env ${ada} '${AMD}' join acme >/dev/null 2>&1 \
    && env ${ada} '${AMD}' join acme 2>&1 | grep -q 'acme-tickets already exists' \
    && git -C acme-tickets diff --quiet && [ \"\$(git -C acme-tickets rev-parse HEAD)\" = \"\${before}\" ]"
assert "join refuses when only the documents directory is there" \
  bash -c "mkdir -p '${people}/ada/docs-only/acme-documents' && cd '${people}/ada/docs-only' \
    && env ${ada} '${AMD}' join acme 2>&1 | grep -q 'acme-documents already exists' \
    && test ! -e acme-tickets"
assert "join refuses inside a git repository" \
  bash -c "cd '${people}/ada/work/acme-tickets' && env ${ada} '${AMD}' join acme 2>&1 | grep -q 'must not be run inside a git repository'"
assert "join refuses a project with no board, leaving no partial clones" \
  bash -c "mkdir -p '${people}/ada/blank' && cd '${people}/ada/blank' \
    && env ${ada} '${AMD}' join blank 2>&1 | grep -q \"isn't an rite project\" \
    && test -z \"\$(ls -A)\""
assert "join refuses a repository that doesn't exist, leaving nothing behind" \
  bash -c "mkdir -p '${people}/ada/missing' && cd '${people}/ada/missing' \
    && ! env ${ada} '${AMD}' join nosuch >/dev/null 2>&1 && test -z \"\$(ls -A)\""
# Four people joining at once: most pushes are rejected the first time and
# must be re-applied on top of whoever won, not dropped with a warning.
for who in bo cy di ed; do
  env_for_who="$(person "${who}" "${who}@t.co")"
  ( cd "${people}/${who}/work" && env ${env_for_who} "${AMD}" join acme >/dev/null 2>&1 ) &
done
wait
assert "four simultaneous joins all end up in the remote's users" \
  bash -c "settings=\"\$(git --git-dir='${origin}/acme/acme-tickets.git' show main:.settings)\" \
    && for who in ada bo cy di ed; do grep -q \"\${who}@t.co\" <<<\"\${settings}\" || exit 1; done"
rm -rf "${origin}" "${people}"

echo "discovery from a subdirectory:"
mkdir -p deep/nested
assert "runs from a subdir (finds repo-root board)" \
  bash -c "cd deep/nested && '${AMD}' board | grep -q TODO"

echo "discovery from a project's parent directory (not itself a git repo):"
# The rite init <project> layout: a plain folder (not a git repo itself)
# holding {project}-tickets as a child. rite board should find it without
# needing to cd one level deeper.
plain_parent="$(mktemp -d)"
mkdir -p "${plain_parent}/solo-tickets/tasks/backlog"
( cd "${plain_parent}/solo-tickets" && git init -q && git config user.email t@t.co && git config user.name t \
  && git add -A && git commit -q -m seed --allow-empty )
assert "rite board finds the one project sibling from its parent directory" \
  bash -c "cd '${plain_parent}' && '${AMD}' board --plain | grep -q TODO"
mkdir -p "${plain_parent}/old-code/tasks/todo" "${plain_parent}/old-code/tasks/doing" "${plain_parent}/old-code/tasks/done"
( cd "${plain_parent}/old-code" && git init -q )
assert "an old v3 board in a code repo beside it isn't a project (ticket 175)" \
  bash -c "cd '${plain_parent}' && '${AMD}' board --plain | grep -q TODO && ! '${AMD}' project 2>&1 | grep -q old-code"
mkdir -p "${plain_parent}/second-tickets/tasks/backlog"
( cd "${plain_parent}/second-tickets" && git init -q && git config user.email t@t.co && git config user.name t )
assert "two project siblings is an ambiguity error, not a guess" \
  bash -c "cd '${plain_parent}' && '${AMD}' board 2>&1 | grep -q 'more than one project board'"
assert "an unrelated empty directory still errors as before" \
  bash -c "cd '${nongit}' && '${AMD}' board 2>&1 | grep -q 'not inside a git repository'"
# Unlike `rite init`, plain board discovery no longer registers anything on
# its own — nothing to clean up here any more.

echo "discovery from inside a sibling repo that has no board of its own (ticket 061):"
# The other real layout: standing inside the *code* repo, a git repository
# in its own right with no tasks/ of its own, sitting next to the real
# tickets repo rather than above it. locate() alone "succeeds" here
# (it never checks the board actually exists), which is exactly what used
# to stop the sibling-scan fallback from ever running.
sibling_parent="$(mktemp -d)"
( cd "${sibling_parent}" && git init -q code && cd code \
  && git config user.email t@t.co && git config user.name t && git commit -q -m seed --allow-empty )
mkdir -p "${sibling_parent}/proj-tickets/tasks/backlog"
( cd "${sibling_parent}/proj-tickets" && git init -q && git config user.email t@t.co && git config user.name t \
  && git add -A && git commit -q -m seed --allow-empty )
assert "rite board finds the sibling tickets repo from inside the code repo" \
  bash -c "cd '${sibling_parent}/code' && '${AMD}' board --plain | grep -q TODO"
mkdir -p "${sibling_parent}/code/deep/nested"
assert "...and from a subdirectory of the code repo, not just its top level" \
  bash -c "cd '${sibling_parent}/code/deep/nested' && '${AMD}' board --plain | grep -q TODO"
mkdir -p "${sibling_parent}/second-tickets/tasks/backlog"
( cd "${sibling_parent}/second-tickets" && git init -q && git config user.email t@t.co && git config user.name t )
# Two *-tickets* siblings is the `rite init`/`rite join` layout — a folder of
# projects — so from a code repository it's an error naming the fix (ticket
# 093), no longer the silent "no board" that used to hide it. Unrelated
# boards that merely share a parent (this suite's own scratch repos in
# /tmp) still fall back quietly; the 093 section below checks that.
assert "two tickets siblings from inside the code repo is an error naming rite project use" \
  bash -c "cd '${sibling_parent}/code' && '${AMD}' board --no-input 2>&1 | grep -q 'rite project use'"
rm -rf "${sibling_parent}"

lonely_parent="$(mktemp -d)"
( cd "${lonely_parent}" && git init -q lonely-code && cd lonely-code \
  && git config user.email t@t.co && git config user.name t )
assert "a code repo with no tickets sibling at all still falls back to today's behaviour" \
  bash -c "cd '${lonely_parent}/lonely-code' && '${AMD}' board --no-input 2>&1 | grep -q 'no board at'"

echo "several projects in one folder (ticket 093):"
# acme and beta side by side, each a tickets + documents pair, plus a code
# repository with no board of its own — the layout two `rite init`s make.
projects="$(mktemp -d)"
(
  set -e
  cd "${projects}"
  for name in acme beta; do
    git init -q "${name}-tickets" && git init -q "${name}-documents"
    ( cd "${name}-tickets" && git config user.email t@t.co && git config user.name t \
      && "${AMD}" init "${name}" --local-only >/dev/null \
      && RITE_COMMIT=0 "${AMD}" new "${name} work" >/dev/null )
  done
  git init -q app
)
assert "from the folder, two projects and no choice is an error naming rite project use" \
  bash -c "cd '${projects}' && ! '${AMD}' board --plain 2>/dev/null && '${AMD}' board 2>&1 | grep -q 'rite project use'"
assert "from a code repository beside them, the same error — not a silent empty board" \
  bash -c "cd '${projects}/app' && '${AMD}' board --no-input 2>&1 | grep -q 'rite project use'"
assert "rite project lists both, none current yet" \
  bash -c "cd '${projects}' && out=\"\$('${AMD}' project)\" && grep -q '^  acme ' <<<\"\${out}\" \
    && grep -q '^  beta ' <<<\"\${out}\" && grep -q 'no current project' <<<\"\${out}\""
assert "rite project use beta makes it current" \
  bash -c "cd '${projects}' && '${AMD}' project use beta | grep -q 'current project: beta' \
    && grep -qx beta .current-project && '${AMD}' project | grep -q '^\* beta '"
assert "the folder then shows beta's board" \
  bash -c "cd '${projects}' && '${AMD}' board --plain | grep -q 'beta work'"
assert "so does the code repository beside it" \
  bash -c "cd '${projects}/app' && '${AMD}' board --plain | grep -q 'beta work'"
assert "inside acme-tickets, acme's own board still wins" \
  bash -c "cd '${projects}/acme-tickets' && '${AMD}' board --plain | grep -q 'acme work'"
assert "rite project use works from inside a tickets repository too" \
  bash -c "cd '${projects}/acme-tickets' && '${AMD}' project use acme >/dev/null && grep -qx acme '${projects}/.current-project'"
assert "rite project use refuses a project that isn't here, changing nothing" \
  bash -c "cd '${projects}' && '${AMD}' project use nosuch 2>&1 | grep -q \"no project 'nosuch'\" && grep -qx acme .current-project"
assert "rite project use with no name and no terminal is a usage error" \
  bash -c "cd '${projects}' && '${AMD}' project use 2>&1 | grep -q 'usage: rite project use'"
assert ".current-project naming a project that's gone is a clear error" \
  bash -c "cd '${projects}' && echo gone >.current-project && '${AMD}' board 2>&1 | grep -q \"current project 'gone'\""
rm -f "${projects}/.current-project"
# A second `rite init` into a folder that already holds one project keeps the
# first one current, so adding a project never changes what rite shows.
second="$(mktemp -d)"
(
  set -e
  cd "${second}"
  for repo in first-tickets first-documents later-tickets later-documents; do
    git init -q "${repo}"
    git -C "${repo}" config user.email t@t.co && git -C "${repo}" config user.name t
  done
  "${AMD}" init first >/dev/null 2>&1
)
assert "init of a second project keeps the first current, and says how to switch" \
  bash -c "cd '${second}' && '${AMD}' init later 2>/dev/null | grep -q 'current is still first; .rite project use later. to switch' \
    && grep -qx first .current-project"
assert "init with a choice already made leaves it alone" \
  bash -c "cd '${second}' && echo later >.current-project && '${AMD}' init first >/dev/null 2>&1 && grep -qx later .current-project"
for repo in first-tickets first-documents later-tickets later-documents; do
  "${AMD}" deregister "${repo}" >/dev/null 2>&1
done
rm -rf "${projects}" "${second}"

echo "two clones of one board (ticket 102):"
# A and B clone one board from a local bare origin, each with its own git
# identity — two people. rite keeps both in step: catching up before every
# command, and replaying or undoing a change whose push lost a race.
two="$(mktemp -d)"
(
  set -e
  git init -q --bare -b main "${two}/origin.git"
  git init -q -b main "${two}/a"
  cd "${two}/a"
  git config user.email a@t.co && git config user.name a && git config commit.gpgsign false
  git remote add origin "${two}/origin.git"
  "${AMD}" init twoclones --local-only >/dev/null 2>&1
  git add -A && git commit -q -m "seed for ticket 102" && git push -q -u origin main 2>/dev/null
  git clone -q "${two}/origin.git" "${two}/b"
  cd "${two}/b" && git config user.email b@t.co && git config user.name b && git config commit.gpgsign false
)
in_a() { (cd "${two}/a" && "${AMD}" "$@"); }
in_b() { (cd "${two}/b" && XDG_STATE_HOME="${two}/state" "${AMD}" "$@"); }
in_a new "From A" --no-edit >/dev/null 2>&1
assert "B sees A's new ticket without a manual pull" \
  bash -c "$(declare -f in_b); two='${two}'; AMD='${AMD}'; in_b board --plain 2>/dev/null | grep -q 'From A'"
assert "B's next ticket is numbered after A's, not alongside it" \
  bash -c "$(declare -f in_b); two='${two}'; AMD='${AMD}'; in_b new 'From B' --no-edit 2>/dev/null | grep -q 'created backlog/002-from-b.md'"
in_a todo 1 --no-branch >/dev/null 2>&1 && in_a lock 1 >/dev/null 2>&1
assert "B can't claim a ticket A holds, and says so with a non-zero exit" \
  bash -c "$(declare -f in_b); two='${two}'; AMD='${AMD}'; ! in_b doing 1 >/dev/null 2>&1 \
    && in_b doing 1 2>&1 | grep -q 'leased by a@t.co'"
in_a new "Third" --no-edit >/dev/null 2>&1
in_b board --plain >/dev/null 2>&1
# One-shot pre-push hook in B: just before B's push, A changes the board
# (with git's per-repository environment cleared, or A's git would act on B).
race() { # the rite arguments A runs at the last moment
  local args
  args="$(printf '%q ' "$@")"
  cat >"${two}/b/.git/hooks/pre-push" <<HOOK
#!/bin/sh
[ -e "${two}/raced" ] && exit 0
touch "${two}/raced"
unset \$(git rev-parse --local-env-vars)
cd "${two}/a" && "${AMD}" ${args} >/dev/null 2>&1
exit 0
HOOK
  chmod +x "${two}/b/.git/hooks/pre-push"
  rm -f "${two}/raced"
}
race backlog 1
assert "a push that lost a race to an unrelated change is replayed on top, exit 0" \
  bash -c "$(declare -f in_b); two='${two}'; AMD='${AMD}'; in_b set 3 priority high 2>&1 | grep -q 'replayed on top'"
assert "...and both changes are on the remote" \
  bash -c "git -C '${two}/origin.git' show main:tasks/backlog/003-third.md | grep -q '^priority: \"high\"' \
    && git -C '${two}/origin.git' ls-tree -r --name-only main | grep -q 'backlog/001-from-a.md'"
race new "A at the same time" --no-edit
assert "a push that clashed (both numbered a new ticket) is replayed, the new ticket renumbered (ticket 197)" \
  bash -c "$(declare -f in_b); two='${two}'; AMD='${AMD}'; in_b new 'B at the same time' --no-edit 2>&1 \
    | grep -q 'is now .*origin had already given out its number'"
assert "...leaving B with A's ticket and its own, both on the remote" \
  bash -c "$(declare -f in_b); two='${two}'; AMD='${AMD}'; out=\"\$(in_b board --plain 2>/dev/null)\" \
    && grep -q 'A at the same time' <<<\"\${out}\" && grep -q 'B at the same time' <<<\"\${out}\" \
    && git -C '${two}/origin.git' ls-tree -r --name-only main | grep -q 'b-at-the-same-time.md'"
assert "...and no ticket id is on the remote twice" \
  bash -c "[ -z \"\$(git -C '${two}/origin.git' ls-tree -r --name-only main | sed -n 's|.*/\([0-9]\{3\}\)-.*|\1|p' | sort | uniq -d)\" ]"
rm -f "${two}/b/.git/hooks/pre-push"
# Ticket 197: origin out of reach. The change waits, rite says so, and once
# origin is back (A having made a ticket meanwhile) B's goes on top.
git -C "${two}/b" remote set-url origin "${two}/nowhere.git"
assert "with origin out of reach a change waits, and listing says how many" \
  bash -c "$(declare -f in_b); two='${two}'; AMD='${AMD}'; in_b new 'Made offline' --no-edit >/dev/null 2>&1; \
    in_b board --plain 2>&1 >/dev/null | grep -q '1 change waiting to push'"
in_a new "Made while B was away" --no-edit >/dev/null 2>&1
git -C "${two}/b" remote set-url origin "${two}/origin.git"
assert "...and when origin is back it goes on top of what origin got meanwhile" \
  bash -c "$(declare -f in_b); two='${two}'; AMD='${AMD}'; in_b board --plain >/dev/null 2>&1; \
    git -C '${two}/origin.git' ls-tree -r --name-only main | grep -q 'made-offline.md' \
    && [ -z \"\$(git -C '${two}/origin.git' ls-tree -r --name-only main | sed -n 's|.*/\([0-9]\{3\}\)-.*|\1|p' | sort | uniq -d)\" ]"
in_a new "Unseen" --no-edit >/dev/null 2>&1
assert "RITE_SYNC=0 leaves the clone alone" \
  bash -c "$(declare -f in_b); two='${two}'; AMD='${AMD}'; ! RITE_SYNC=0 in_b board --plain 2>/dev/null | grep -q 'Unseen'"
assert "cloud ticket numbering explains it needs a plan, and creates nothing" \
  bash -c "$(declare -f in_a); two='${two}'; AMD='${AMD}'; RITE_TICKET_NUMBERING=cloud in_a new 'Cloudy' --no-edit 2>&1 | grep -q 'needs a KanRite plan' \
    && ! ls '${two}'/a/tasks/backlog/*cloudy* >/dev/null 2>&1"
rm -rf "${two}"

echo "a ticket's repository (tickets 101, 107):"
# A board with a code repository beside it, registered so it counts as
# cloned here; another declared but never cloned.
rp="$(mktemp -d)"
(
  set -e
  cd "${rp}"
  git init -q -b main board && git init -q -b main code
  for r in board code; do
    git -C "${r}" config user.email t@t.co && git -C "${r}" config user.name t && git -C "${r}" config commit.gpgsign false
  done
  git -C code commit -q --allow-empty -m "seed for ticket 107"
  cd board && RITE_COMMIT=0 "${AMD}" init repotest --local-only >/dev/null 2>&1
  RITE_COMMIT=0 "${AMD}" register "${rp}/code" >/dev/null 2>&1
)
in_rp() { (cd "${rp}/board" && RITE_COMMIT=0 "${AMD}" "$@"); }
assert "rite new --repository names the repository the work is in" \
  bash -c "$(declare -f in_rp); rp='${rp}'; AMD='${AMD}'; in_rp new 'Code change' --repository code --branch-type feature --no-edit >/dev/null 2>&1 \
    && grep -qx 'repository: \"code\"' '${rp}'/board/tasks/backlog/001-code-change.md"
assert "...and refuses one that isn't cloned on this machine" \
  bash -c "$(declare -f in_rp); rp='${rp}'; AMD='${AMD}'; in_rp new 'Elsewhere' --repository nowhere --no-edit 2>&1 | grep -q \"nowhere isn't cloned on this machine\" \
    && ! ls '${rp}'/board/tasks/backlog/*elsewhere* >/dev/null 2>&1"
assert "a new ticket without one leaves the field empty" \
  bash -c "$(declare -f in_rp); rp='${rp}'; AMD='${AMD}'; in_rp new 'Board chore' --no-edit >/dev/null 2>&1 \
    && grep -qx 'repository: \"\"' '${rp}'/board/tasks/backlog/002-board-chore.md"
assert "rite set <ref> repository points an existing ticket at one" \
  bash -c "$(declare -f in_rp); rp='${rp}'; AMD='${AMD}'; in_rp set 2 repository code 2>/dev/null | grep -q '002 repository code' \
    && grep -qx 'repository: \"code\"' '${rp}'/board/tasks/backlog/002-board-chore.md"
assert "...an owner/name value is the same repository" \
  bash -c "$(declare -f in_rp); rp='${rp}'; AMD='${AMD}'; in_rp set 2 repository someorg/code 2>/dev/null | grep -q '002 repository code'"
assert "...and an empty value clears it" \
  bash -c "$(declare -f in_rp); rp='${rp}'; AMD='${AMD}'; in_rp set 2 repository '' >/dev/null 2>&1 \
    && grep -qx 'repository: \"\"' '${rp}'/board/tasks/backlog/002-board-chore.md"
assert "...but a repository not cloned here is refused" \
  bash -c "$(declare -f in_rp); rp='${rp}'; AMD='${AMD}'; ! in_rp set 2 repository nowhere >/dev/null 2>&1"
assert "entering todo makes the ticket's branch in its repository's clone" \
  bash -c "$(declare -f in_rp); rp='${rp}'; AMD='${AMD}'; in_rp todo 1 2>&1 | grep -q 'branch feature/code-change in code' \
    && [ \"\$(git -C '${rp}/code' rev-parse --abbrev-ref HEAD)\" = feature/code-change ] \
    && [ \"\$(git -C '${rp}/board' symbolic-ref --short HEAD)\" = main ]"
"${AMD}" deregister code >/dev/null 2>&1
rm -rf "${rp}"

echo "a tickets repository holds no code branches (ticket 053):"
tr="$(mktemp -d)"
( cd "${tr}" && git init -q -b main demo-tickets && git init -q -b main demo-api && cd demo-tickets \
  && git config user.email t@t.co && git config user.name t && git config commit.gpgsign false \
  && RITE_COMMIT=0 "${AMD}" init demo --local-only >/dev/null 2>&1 \
  && XDG_CONFIG_HOME="${tr}/config" RITE_COMMIT=0 "${AMD}" register ../demo-api >/dev/null 2>&1 \
  && RITE_COMMIT=0 "${AMD}" new 'Needs code' --branch-type feature --no-edit >/dev/null 2>&1 )
in_tr() { (cd "${tr}/demo-tickets" && XDG_CONFIG_HOME="${tr}/config" RITE_COMMIT=0 "${AMD}" "$@"); }
assert "a ticket that wants a branch but names no repository is refused, unmoved" \
  bash -c "$(declare -f in_tr); tr='${tr}'; AMD='${AMD}'; ! in_tr todo 1 >/dev/null 2>&1 \
    && in_tr todo 1 2>&1 | grep -q 'names no repository' \
    && ls '${tr}'/demo-tickets/tasks/backlog/001-needs-code.md >/dev/null \
    && [ \"\$(git -C '${tr}/demo-tickets' symbolic-ref --short HEAD)\" = main ]"
assert "...and it's a draft (ticket 066), so --no-branch can't move it either" \
  bash -c "$(declare -f in_tr); tr='${tr}'; AMD='${AMD}'; ! in_tr todo 1 --no-branch >/dev/null 2>&1 \
    && grep -q '^tags: \[DRAFT\]' '${tr}'/demo-tickets/tasks/backlog/001-needs-code.md"

echo "a repository and a branch type go together (ticket 066):"
assert "rite new with a repository and no branch type makes a draft, and says why" \
  bash -c "$(declare -f in_tr); tr='${tr}'; AMD='${AMD}'; \
    in_tr new 'Fix the api' --repository demo-api --no-edit 2>&1 | grep -q 'draft until.*no branch type' \
    && grep -q '^tags: \[DRAFT\]' '${tr}'/demo-tickets/tasks/backlog/002-fix-the-api.md"
assert "with --branch-type none it isn't a draft, has no branch name, and todo says nothing of a branch" \
  bash -c "$(declare -f in_tr); tr='${tr}'; AMD='${AMD}'; \
    in_tr new 'Release it' --repository demo-api --branch-type none --no-edit >/dev/null 2>&1 \
    && f='${tr}'/demo-tickets/tasks/backlog/003-release-it.md && ! grep -q DRAFT \"\$f\" \
    && grep -q '^branch-name: \"\"' \"\$f\" \
    && out=\"\$(in_tr todo 3 2>&1)\" && ! echo \"\$out\" | grep -q 'branch' \
    && [ \"\$(git -C '${tr}/demo-api' symbolic-ref --short HEAD)\" = main ]"
assert "rite ready refuses a draft until its repository and branch type go together" \
  bash -c "$(declare -f in_tr); tr='${tr}'; AMD='${AMD}'; \
    in_tr ready 2 2>&1 | grep -q \"can't be ready yet\" \
    && in_tr set 2 branch-type bugfix 2>&1 | grep -q '002 branch type bugfix' \
    && grep -q '^branch-name: \"bugfix/fix-the-api\"' '${tr}'/demo-tickets/tasks/backlog/002-fix-the-api.md \
    && grep -q DRAFT '${tr}'/demo-tickets/tasks/backlog/002-fix-the-api.md \
    && in_tr ready 2 >/dev/null && ! grep -q DRAFT '${tr}'/demo-tickets/tasks/backlog/002-fix-the-api.md"
assert "rite set repository past the backlog with no branch type is refused, unchanged" \
  bash -c "$(declare -f in_tr); tr='${tr}'; AMD='${AMD}'; \
    in_tr new 'Plain' --no-edit >/dev/null 2>&1 && in_tr todo 4 >/dev/null 2>&1 \
    && in_tr set 4 repository demo-api 2>&1 | grep -q 'past the backlog' \
    && grep -q '^repository: \"\"' '${tr}'/demo-tickets/tasks/todo/004-plain.md"
assert "rite set repository on a backlog ticket with no branch type makes it a draft" \
  bash -c "$(declare -f in_tr); tr='${tr}'; AMD='${AMD}'; \
    in_tr new 'Later' --no-edit >/dev/null 2>&1 \
    && in_tr set 5 repository demo-api 2>&1 | grep -q 'draft until' \
    && grep -q DRAFT '${tr}'/demo-tickets/tasks/backlog/005-later.md"
assert "rite set gives a todo ticket a repository and a branch type at once, in one commit (ticket 258)" \
  bash -c "$(declare -f in_tr); tr='${tr}'; AMD='${AMD}'; \
    in_tr new 'Both at once' --no-edit >/dev/null 2>&1 && in_tr todo 6 >/dev/null 2>&1 \
    && before=\"\$(git -C '${tr}/demo-tickets' rev-list --count HEAD 2>/dev/null || echo 0)\" \
    && out=\"\$(cd '${tr}/demo-tickets' && XDG_CONFIG_HOME='${tr}/config' '${AMD}' set 6 repository demo-api branch-type feature 2>&1)\" \
    && echo \"\$out\" | grep -q '006 repository demo-api' && echo \"\$out\" | grep -q '006 branch type feature' \
    && f='${tr}'/demo-tickets/tasks/todo/006-both-at-once.md \
    && grep -q '^repository: \"demo-api\"' \"\$f\" && grep -q '^branch-type: \"feature\"' \"\$f\" \
    && grep -q '^branch-name: \"feature/both-at-once\"' \"\$f\" && ! grep -q DRAFT \"\$f\" \
    && [ \"\$(git -C '${tr}/demo-tickets' rev-list --count HEAD)\" = \"\$((before + 1))\" ] \
    && [ -z \"\$(git -C '${tr}/demo-tickets' status --porcelain -- \"\$f\")\" ]"
assert "...still refused when the pair would be left mismatched, changing nothing" \
  bash -c "$(declare -f in_tr); tr='${tr}'; AMD='${AMD}'; \
    in_tr set 6 repository '' title 'Renamed' 2>&1 | grep -q 'past the backlog' \
    && grep -q '^title: \"Both at once\"' '${tr}'/demo-tickets/tasks/todo/006-both-at-once.md"
assert "rite set refuses an unknown or repeated field, and requires with others" \
  bash -c "$(declare -f in_tr); tr='${tr}'; AMD='${AMD}'; \
    in_tr set 6 points 3 colour red 2>&1 | grep -q \"colour isn't a field\" \
    && in_tr set 6 points 3 points 5 2>&1 | grep -q 'points is given twice' \
    && in_tr set 6 requires 1 points 3 2>&1 | grep -q 'set requires on its own' \
    && ! grep -q '^points: \"3\"' '${tr}'/demo-tickets/tasks/todo/006-both-at-once.md"
sed -i 's/^repository: \"\"/repository: \"demo-api\"/' "${tr}"/demo-tickets/tasks/todo/004-plain.md
tr_before="$(sha1sum < "${tr}"/demo-tickets/tasks/todo/004-plain.md)"
assert "rite check lists a ticket mismatched by hand, and changes nothing" \
  bash -c "$(declare -f in_tr); tr='${tr}'; AMD='${AMD}'; out=\"\$(in_tr check 2>/dev/null)\" \
    && echo \"\$out\" | grep -q '^004 todo/ Plain: it names a repository (demo-api) but no branch type' \
    && echo \"\$out\" | grep -q '^001 backlog/' && echo \"\$out\" | grep -q '^005 backlog/' && echo \"\$out\" | grep -q '^3 to settle' \
    && [ \"\$(sha1sum < '${tr}'/demo-tickets/tasks/todo/004-plain.md)\" = '${tr_before}' ]"
assert "the branch-type completions and rite set offer none" \
  bash -c "'${AMD}' completions fish 2>/dev/null | grep -q 'none' && '${AMD}' set --help | grep -q 'branch-type'"
pl="$(mktemp -d)"
assert "a board inside its code repository is unaffected" \
  bash -c "cd '${pl}' && git init -q && git config user.email t@t.co && git config user.name t \
    && RITE_YES=1 RITE_COMMIT=0 '${AMD}' init plain --local-only >/dev/null \
    && RITE_COMMIT=0 '${AMD}' new 'Code work' --branch-type feature --no-edit >/dev/null 2>&1 \
    && ! grep -q DRAFT tasks/backlog/001-code-work.md \
    && RITE_COMMIT=0 '${AMD}' check | grep -q 'inside its code repository'"
rm -rf "${pl}"
rm -rf "${tr}"

echo "register in a folder of several projects (ticket 103):"
# acme (current) and beta side by side, and a code repository, beta-app,
# that belongs to beta. From inside beta-app, a plain `rite register` would
# declare it on acme, the folder's current project.
rg="$(mktemp -d)"
rg_env="XDG_CONFIG_HOME=${rg}/config"
(
  set -e
  export XDG_CONFIG_HOME="${rg}/config"
  cd "${rg}"
  for name in acme beta; do
    git init -q "${name}-tickets"
    ( cd "${name}-tickets" && git config user.email t@t.co && git config user.name t \
      && RITE_COMMIT=0 "${AMD}" init "${name}" --local-only >/dev/null 2>&1 )
  done
  git init -q beta-app
  "${AMD}" project use acme >/dev/null
)
assert "from a code repo, register refuses without a terminal, naming the folder's current project" \
  bash -c "cd '${rg}/beta-app' && ! env ${rg_env} '${AMD}' register >/dev/null 2>&1 \
    && env ${rg_env} '${AMD}' register 2>&1 | grep -q \"would be declared on project 'acme'\""
assert "...suggests the project the repository's name points at" \
  bash -c "cd '${rg}/beta-app' && env ${rg_env} '${AMD}' register 2>&1 | grep -q 'did you mean --project beta'"
assert "...and declares nothing" \
  bash -c "! grep -q 'beta-app' '${rg}/acme-tickets/.settings' && ! grep -q 'beta-app' '${rg}/beta-tickets/.settings'"
assert "register --project beta declares it on beta" \
  bash -c "cd '${rg}/beta-app' && env ${rg_env} '${AMD}' register --project beta | grep -q 'registered beta-app' \
    && grep -q '^repos = .*beta-app' '${rg}/beta-tickets/.settings' && ! grep -q 'beta-app' '${rg}/acme-tickets/.settings'"
assert "register --project with a name that isn't here is refused" \
  bash -c "cd '${rg}/beta-app' && env ${rg_env} '${AMD}' register --project nosuch 2>&1 | grep -q \"no project 'nosuch'\""
assert "from inside a tickets repository, register works as before" \
  bash -c "cd '${rg}/acme-tickets' && env ${rg_env} '${AMD}' register '${rg}/beta-app' | grep -q 'beta-app'"
# beta-app is now declared on both acme and beta. deregister takes it off a
# project for everyone, so it is guarded the same way (ticket 120).
assert "from a code repo, deregister refuses without a terminal, naming the folder's current project" \
  bash -c "cd '${rg}/beta-app' && env ${rg_env} '${AMD}' deregister beta-app 2>&1 | grep -q \"would be taken off project 'acme'\" \
    && grep -q '^repos = .*beta-app' '${rg}/acme-tickets/.settings' && grep -q '^repos = .*beta-app' '${rg}/beta-tickets/.settings'"
assert "deregister --project beta takes it off beta only" \
  bash -c "cd '${rg}/beta-app' && env ${rg_env} '${AMD}' deregister beta-app --project beta | grep -q 'no longer part of the project' \
    && ! grep -q 'beta-app' '${rg}/beta-tickets/.settings' && grep -q '^repos = .*beta-app' '${rg}/acme-tickets/.settings'"
assert "from inside a tickets repository, deregister takes it off that project" \
  bash -c "cd '${rg}/acme-tickets' && env ${rg_env} '${AMD}' deregister beta-app | grep -q 'no longer part of the project' \
    && ! grep -q 'beta-app' '${rg}/acme-tickets/.settings'"
assert "...and once nothing declares or registers it, deregister refuses" \
  bash -c "cd '${rg}/acme-tickets' && ! env ${rg_env} '${AMD}' deregister beta-app 2>/dev/null \
    && env ${rg_env} '${AMD}' deregister beta-app 2>&1 | grep -q 'neither declared for this project nor registered'"
rm -rf "${rg}"

echo "tags (ticket 122):"
tg="$(mktemp -d)"
tg_env="XDG_CONFIG_HOME=${tg}/config RITE_NO_INPUT=1"
(
  set -e
  cd "${tg}" && git init -q tags-tickets && cd tags-tickets
  git config user.email t@t.co && git config user.name t
  env ${tg_env} "${AMD}" init tags --local-only >/dev/null 2>&1
)
tgb="${tg}/tags-tickets"
assert "with nothing listed, rite tags says only the built-ins are there" \
  bash -c "cd '${tgb}' && env ${tg_env} '${AMD}' tags | grep -q 'only the built-in DRAFT and TRIAGE'"
assert "...and a new ticket can't carry any other tag (ticket 167)" \
  bash -c "cd '${tgb}' && ! env ${tg_env} '${AMD}' new 'Unlisted' -t misc --no-edit 2>/dev/null \
    && env ${tg_env} '${AMD}' new 'Unlisted' -t misc --no-edit 2>&1 | grep -q \"not on the project's tag list: MISC\" \
    && ! ls tasks/backlog/*-unlisted.md 2>/dev/null"
assert "rite tags add saves the tag in UPPERCASE in .settings" \
  bash -c "cd '${tgb}' && env ${tg_env} '${AMD}' tags add ' future ' | grep -qx 'added FUTURE' && grep -qx 'tags = FUTURE' .settings"
assert "...refuses one already listed, ignoring case" \
  bash -c "cd '${tgb}' && ! env ${tg_env} '${AMD}' tags add Future 2>/dev/null"
assert "...and one that isn't one word" \
  bash -c "cd '${tgb}' && ! env ${tg_env} '${AMD}' tags add 'two words' 2>/dev/null"
assert "arbitrary tags are prevented by default once there's a list" \
  bash -c "cd '${tgb}' && env ${tg_env} '${AMD}' tags | grep -q 'only listed tags: on' \
    && ! env ${tg_env} '${AMD}' new 'Prevented' -t misc --no-edit 2>/dev/null && ! ls tasks/backlog/*-prevented.md 2>/dev/null"
assert "rite new saves tags in UPPERCASE and, with arbitrary tags allowed, warns about an unlisted one" \
  bash -c "cd '${tgb}' && env ${tg_env} '${AMD}' tags only off | grep -q 'only listed tags: off' \
    && env ${tg_env} '${AMD}' new 'Tagged' -t future -t misc --no-edit 2>&1 | grep -q \"not on the project's tag list: MISC\" \
    && grep -q '^tags: \[FUTURE,MISC\]' tasks/backlog/*-tagged.md"
assert "rite tag matches without regard to case" \
  bash -c "cd '${tgb}' && env ${tg_env} '${AMD}' tag future --plain | grep -q 'Tagged'"
assert "rite tags lists counts, and tags in use but not listed" \
  bash -c "cd '${tgb}' && out=\$(env ${tg_env} '${AMD}' tags) && printf '%s\\n' \"\$out\" | grep -q '^FUTURE.1\$' \
    && printf '%s\\n' \"\$out\" | grep -q 'in use but not listed: MISC (1)'"
assert "a tag a ticket carries can't be removed" \
  bash -c "cd '${tgb}' && env ${tg_env} '${AMD}' tags remove future 2>&1 | grep -q 'still on 1 ticket' && grep -q 'tags = FUTURE' .settings"
assert "an unused tag can" \
  bash -c "cd '${tgb}' && env ${tg_env} '${AMD}' tags add old >/dev/null && env ${tg_env} '${AMD}' tags remove OLD | grep -qx 'removed OLD' \
    && grep -qx 'tags = FUTURE' .settings"
assert "with only listed tags on, rite new refuses an unlisted tag" \
  bash -c "cd '${tgb}' && env ${tg_env} '${AMD}' tags only on | grep -q 'only listed tags: on' \
    && ! env ${tg_env} '${AMD}' new 'Refused' -t misc --no-edit 2>/dev/null && ! ls tasks/backlog/*-refused.md 2>/dev/null \
    && env ${tg_env} '${AMD}' new 'Allowed' -t future --no-edit >/dev/null 2>&1"
assert "--tag -TAG leaves out tickets carrying it (ticket 123)" \
  bash -c "cd '${tgb}' && out=\$(env ${tg_env} '${AMD}' board --plain --tag -MISC) \
    && printf '%s\\n' \"\$out\" | grep -q 'Allowed' && ! printf '%s\\n' \"\$out\" | grep -q 'Tagged'"
assert "...and --tag can be repeated: tagged FUTURE and not MISC" \
  bash -c "cd '${tgb}' && out=\$(env ${tg_env} '${AMD}' board --plain --tag future --tag -misc) \
    && printf '%s\\n' \"\$out\" | grep -q 'Allowed' && ! printf '%s\\n' \"\$out\" | grep -q 'Tagged'"
assert "...two wanted tags need just one: --tag MISC --tag FUTURE shows both" \
  bash -c "cd '${tgb}' && out=\$(env ${tg_env} '${AMD}' board --plain --tag MISC --tag FUTURE) \
    && printf '%s\\n' \"\$out\" | grep -q 'Allowed' && printf '%s\\n' \"\$out\" | grep -q 'Tagged'"
assert "...and a bare destination verb takes -TAG too" \
  bash -c "cd '${tgb}' && out=\$(env ${tg_env} '${AMD}' backlog --plain --tag -MISC) \
    && printf '%s\\n' \"\$out\" | grep -q 'Allowed' && ! printf '%s\\n' \"\$out\" | grep -q 'Tagged'"

echo "drafts (ticket 114):"
assert "rite new --draft tags it DRAFT, even with only listed tags on" \
  bash -c "cd '${tgb}' && env ${tg_env} '${AMD}' new 'Draft idea' --draft --no-edit >/dev/null 2>&1 \
    && grep -q '^tags: \[DRAFT\]' tasks/backlog/*-draft-idea.md"
dr_id() { basename "$(ls "${tgb}"/tasks/backlog/*-draft-idea.md "${tgb}"/tasks/todo/*-draft-idea.md 2>/dev/null)" | cut -c1-3; }
assert "a draft can't leave the backlog" \
  bash -c "cd '${tgb}' && env ${tg_env} RITE_NO_BRANCH=1 '${AMD}' todo $(dr_id) 2>&1 | grep -q 'is a draft' \
    && ls tasks/backlog/*-draft-idea.md >/dev/null"
assert "rite draft on a draft says so and changes nothing" \
  bash -c "cd '${tgb}' && env ${tg_env} '${AMD}' draft $(dr_id) | grep -q 'already a draft'"
assert "rite ready takes it out of draft, and then it moves on" \
  bash -c "cd '${tgb}' && env ${tg_env} '${AMD}' ready $(dr_id) | grep -q 'is ready' \
    && ! grep -q 'DRAFT' tasks/backlog/*-draft-idea.md \
    && env ${tg_env} RITE_NO_BRANCH=1 '${AMD}' todo $(dr_id) >/dev/null 2>&1 && ls tasks/todo/*-draft-idea.md >/dev/null"
assert "rite tag DRAFT lists the drafts" \
  bash -c "cd '${tgb}' && env ${tg_env} '${AMD}' new 'Second draft' --draft --no-edit >/dev/null 2>&1 \
    && env ${tg_env} '${AMD}' tag DRAFT | grep -q 'Second draft'"
assert "rite tags shows the built-ins on their own line, not as unlisted" \
  bash -c "cd '${tgb}' && out=\$(env ${tg_env} '${AMD}' tags) && printf '%s\\n' \"\$out\" | grep -q '^built in: DRAFT ([0-9]*), TRIAGE (0)\$' \
    && ! printf '%s\\n' \"\$out\" | grep -q 'not listed: .*DRAFT'"
assert "DRAFT is built in: it can't be added to or removed from the list" \
  bash -c "cd '${tgb}' && ! env ${tg_env} '${AMD}' tags add draft 2>/dev/null \
    && env ${tg_env} '${AMD}' tags remove DRAFT 2>&1 | grep -q 'built in'"
rm -rf "${tg}"

echo "GitHub issues into the backlog (ticket 158):"
# A fake gh on PATH: records its arguments and prints two open issues.
gi="$(mktemp -d)"
mkdir -p "${gi}/bin"
cat > "${gi}/bin/gh" <<'GH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_LOG:?}"
cat <<'JSON'
[{"number": 4, "title": "Checkout button overlaps", "body": "On small phones.", "labels": [{"name": "bug"}, {"name": "needs-info"}],
  "url": "https://github.com/acme/web/issues/4", "author": {"login": "sam"}},
 {"number": 2, "title": "Dark mode", "body": "", "labels": [], "url": "https://github.com/acme/web/issues/2", "author": {"login": "jo"}}]
JSON
GH
chmod +x "${gi}/bin/gh"
gi_env="XDG_CONFIG_HOME=${gi}/config RITE_NO_INPUT=1 GH_LOG=${gi}/gh.log PATH=${gi}/bin:${PATH}"
(
  set -e
  cd "${gi}" && git init -q -b main issues-tickets && cd issues-tickets
  git config user.email t@t.co && git config user.name t && git config commit.gpgsign false
  env ${gi_env} "${AMD}" init issues --local-only >/dev/null 2>&1
  env ${gi_env} "${AMD}" tags add bug >/dev/null
)
gib="${gi}/issues-tickets"
assert "rite issues --dry-run lists what it would import and writes nothing" \
  bash -c "cd '${gib}' && out=\$(env ${gi_env} '${AMD}' issues --repo acme/web --dry-run) \
    && printf '%s\\n' \"\$out\" | grep -q '#2 -> 001 \"Dark mode\" (acme/web) \[TRIAGE\] top\$' \
    && printf '%s\\n' \"\$out\" | grep -q '#4 -> 002 \"Checkout button overlaps\" (acme/web) \[TRIAGE, BUG\] top, labels not on the tag list: needs-info' \
    && printf '%s\\n' \"\$out\" | grep -q 'would import 2 issues — nothing written' \
    && ! ls tasks/backlog/*.md 2>/dev/null | grep -v README"
assert "...a real run makes TRIAGE tickets with their source, in one commit" \
  bash -c "cd '${gib}' && before=\$(git rev-list --count HEAD 2>/dev/null || echo 0) \
    && env ${gi_env} '${AMD}' issues --repo acme/web | grep -q 'imported 2 issues' \
    && grep -q '^tags: \[TRIAGE,BUG\]' tasks/backlog/002-checkout-button-overlaps.md \
    && grep -q '^source: \"github:acme/web#4\"' tasks/backlog/002-checkout-button-overlaps.md \
    && grep -q 'Labels not on the project.s tag list: needs-info' tasks/backlog/002-checkout-button-overlaps.md \
    && grep -q 'On small phones.' tasks/backlog/002-checkout-button-overlaps.md \
    && [ \"\$(git rev-list --count HEAD)\" = \$((before + 1)) ] \
    && git log -1 --format=%s | grep -q 'import: issues \"2 from acme/web\"'"
assert "...a second run imports nothing" \
  bash -c "cd '${gib}' && env ${gi_env} '${AMD}' issues --repo acme/web | grep -q 'nothing new to import, 2 already imported'"
assert "...nor once its ticket is abandoned" \
  bash -c "cd '${gib}' && env ${gi_env} '${AMD}' abandon 1 >/dev/null 2>&1 && rm -rf tasks/archive \
    && env ${gi_env} '${AMD}' issues --repo acme/web | grep -q '2 already imported' && ! ls tasks/backlog/003-* 2>/dev/null"
assert "--label is passed to gh, and rite import issues points at rite issues (ticket 173)" \
  bash -c "cd '${gib}' && env ${gi_env} '${AMD}' issues --repo acme/web --label bug --dry-run >/dev/null \
    && tail -1 '${gi}/gh.log' | grep -q -- '--repo acme/web .*--label bug' \
    && env ${gi_env} '${AMD}' import issues 2>&1 | grep -q 'pulled in automatically.*rite issues. checks now'"
assert "TRIAGE is built in: it can't be added to or removed from the list" \
  bash -c "cd '${gib}' && ! env ${gi_env} '${AMD}' tags add triage 2>/dev/null \
    && env ${gi_env} '${AMD}' tags remove TRIAGE 2>&1 | grep -q 'built in'"
assert "a TRIAGE ticket isn't a draft" \
  bash -c "cd '${gib}' && env ${gi_env} RITE_NO_BRANCH=1 '${AMD}' todo 2 >/dev/null 2>&1 && ls tasks/todo/002-* >/dev/null"
assert "a TRIAGE ticket is always top, and its priority can't be lowered (tickets 248, 252)" \
  bash -c "cd '${gib}' && grep -q '^priority: \"top\"' tasks/backlog/002-checkout-button-overlaps.md 2>/dev/null \
      || grep -q '^priority: \"top\"' tasks/todo/002-checkout-button-overlaps.md \
    && env ${gi_env} '${AMD}' set 2 priority low 2>&1 | grep -q 'waiting to be triaged, so it.s top' \
    && env ${gi_env} '${AMD}' triage-priority | grep -q '^top' \
    && ! env ${gi_env} '${AMD}' triage-priority low 2>/dev/null"
assert "with no repository on GitHub, it asks for owner/repo" \
  bash -c "cd '${gib}' && env ${gi_env} '${AMD}' issues 2>&1 | grep -q 'rite issues --repo owner/repo'"
assert "rite issues off and on set whether the landing page pulls them in (ticket 173)" \
  bash -c "cd '${gib}' && env ${gi_env} '${AMD}' issues off | grep -q 'only pulled in by .rite issues.' \
    && grep -qx 'import-issues = off' .settings \
    && env ${gi_env} '${AMD}' issues on | grep -q 'landing page pulls in' && grep -qx 'import-issues = on' .settings"
rm -rf "${gi}"

echo "the registry is scoped by project directory (ticket 095):"
# Two folders, each with a project called p: p-tickets and p-code. Both
# p-codes are registered on this machine; each board must use its own.
sc="$(mktemp -d)"
(
  set -e
  export XDG_CONFIG_HOME="${sc}/config" RITE_COMMIT=0
  for dir in one two; do
    mkdir -p "${sc}/${dir}" && cd "${sc}/${dir}"
    for r in p-tickets p-code; do
      git init -q -b main "${r}"
      git -C "${r}" config user.email t@t.co && git -C "${r}" config user.name t && git -C "${r}" config commit.gpgsign false
    done
    git -C p-code commit -q --allow-empty -m "seed for ticket 095 (${dir})"
    ( cd p-tickets && "${AMD}" init p --local-only >/dev/null 2>&1 && "${AMD}" register ../p-code >/dev/null 2>&1 )
  done
)
in_sc() { (cd "${sc}/$1/p-tickets" && shift && env XDG_CONFIG_HOME="${sc}/config" RITE_COMMIT=0 "${AMD}" "$@"); }
assert "both folders' p-code are registered side by side" \
  bash -c "[ \"\$(grep -c '/p-code\$' '${sc}/config/kanrite/repos')\" -eq 2 ]"
assert "a ticket in folder one makes its branch in one's own p-code" \
  bash -c "$(declare -f in_sc); sc='${sc}'; AMD='${AMD}'; in_sc one new 'From one' --repository p-code --branch-type feature --no-edit >/dev/null 2>&1 \
    && in_sc one todo 1 >/dev/null 2>&1 \
    && git -C '${sc}/one/p-code' rev-parse --verify -q feature/from-one >/dev/null \
    && ! git -C '${sc}/two/p-code' rev-parse --verify -q feature/from-one >/dev/null"
assert "...and one in folder two, in two's" \
  bash -c "$(declare -f in_sc); sc='${sc}'; AMD='${AMD}'; in_sc two new 'From two' --repository p-code --branch-type feature --no-edit >/dev/null 2>&1 \
    && in_sc two todo 1 >/dev/null 2>&1 \
    && git -C '${sc}/two/p-code' rev-parse --verify -q feature/from-two >/dev/null \
    && ! git -C '${sc}/one/p-code' rev-parse --verify -q feature/from-two >/dev/null"
# One remote, cloned in two places.
git init -q --bare -b main "${sc}/shared.git"
mkdir -p "${sc}/clones/a" "${sc}/clones/b"
git clone -q "${sc}/shared.git" "${sc}/clones/a/shared" 2>/dev/null
git clone -q "${sc}/shared.git" "${sc}/clones/b/shared" 2>/dev/null
in_sc one register "${sc}/clones/a/shared" >/dev/null 2>&1
assert "registering a second clone of one remote warns, naming the first" \
  bash -c "$(declare -f in_sc); sc='${sc}'; AMD='${AMD}'; out=\"\$(in_sc one register '${sc}/clones/b/shared' 2>&1)\" \
    && grep -q \"shared is also cloned at ${sc}/clones/a/shared\" <<<\"\${out}\" \
    && grep -q 'registered shared' <<<\"\${out}\""
assert "a clone of a different remote doesn't" \
  bash -c "$(declare -f in_sc); sc='${sc}'; AMD='${AMD}'; ! in_sc one register '${sc}/one/p-code' 2>&1 | grep -q 'also cloned'"
rm -rf "${sc}/clones/a"
in_sc one register "${sc}/clones/b/shared" >/dev/null 2>&1
assert "a registered clone that's gone is dropped from the registry" \
  bash -c "! grep -q 'clones/a' '${sc}/config/kanrite/repos' && grep -q 'clones/b/shared' '${sc}/config/kanrite/repos'"
rm -rf "${sc}"

echo "rite clone fetches a project's missing repositories (ticket 108):"
# Remotes for p-tickets and three code repositories, side by side. The
# maker declares all three; a teammate has only cloned the tickets repo.
cl="$(mktemp -d)"
(
  set -e
  export XDG_CONFIG_HOME="${cl}/maker-config" RITE_COMMIT=0
  for r in p-tickets code extra more; do
    git init -q --bare -b main "${cl}/remote/${r}.git"
    git clone -q "${cl}/remote/${r}.git" "${cl}/maker/${r}" 2>/dev/null
    cd "${cl}/maker/${r}"
    git config user.email t@t.co && git config user.name t && git config commit.gpgsign false
    if [ "${r}" = p-tickets ]; then
      "${AMD}" init p --local-only >/dev/null 2>&1
      printf 'repos = code,extra,more\n' >>.settings
      git add -A
    fi
    git commit -q --allow-empty -m "seed for ticket 108"
    git push -q origin HEAD:main 2>/dev/null
  done
  mkdir -p "${cl}/mate" && git clone -q "${cl}/remote/p-tickets.git" "${cl}/mate/p-tickets" 2>/dev/null
  git -C "${cl}/mate/p-tickets" config user.email m@t.co && git -C "${cl}/mate/p-tickets" config user.name m
)
in_cl() { (cd "${cl}/mate/p-tickets" && env XDG_CONFIG_HOME="${cl}/mate-config" RITE_COMMIT=0 "${AMD}" "$@"); }
assert "rite clone lists the declared repositories that aren't here" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; out=\"\$(in_cl clone 2>/dev/null)\" \
    && grep -qx '  code' <<<\"\${out}\" && grep -qx '  extra' <<<\"\${out}\" && grep -qx '  more' <<<\"\${out}\""
assert "rite clone <name> clones it beside the tickets repo, from the same place" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; in_cl clone code 2>/dev/null \
    | grep -q \"cloned code into ${cl}/mate/code (from ${cl}/remote/code.git)\" \
    && [ \"\$(git -C '${cl}/mate/code' log -1 --format=%s)\" = 'seed for ticket 108' ]"
assert "...registers it, so a ticket can now name it" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; grep -q '/mate/code\$' '${cl}/mate-config/kanrite/repos' \
    && in_cl new 'Work on code' --repository code --no-edit >/dev/null 2>&1"
assert "...and isn't listed as missing any more" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; ! in_cl clone 2>/dev/null | grep -qx '  code'"
assert "cloning it again is refused" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; in_cl clone code 2>&1 | grep -q 'code is already cloned here'"
assert "a name the project doesn't declare is refused" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; in_cl clone nosuch 2>&1 | grep -q \"nosuch isn't declared for this project\" \
    && test ! -e '${cl}/mate/nosuch'"
mkdir "${cl}/mate/extra"
assert "a directory already in the way is refused, and left alone" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; in_cl clone extra 2>&1 | grep -q 'already exists' \
    && test -d '${cl}/mate/extra' && test ! -e '${cl}/mate/extra/.git'"
rmdir "${cl}/mate/extra"
assert "rite clone --all clones every one still missing" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; in_cl clone --all >/dev/null 2>&1 \
    && test -d '${cl}/mate/extra/.git' && test -d '${cl}/mate/more/.git' \
    && in_cl clone 2>/dev/null | grep -q 'every declared repository is cloned here'"
rm -rf "${cl}"

echo "rite repo new makes a repository for the project (ticket 228):"
# A tickets repository cloned from a local bare remote: new repositories go
# beside that remote, as they would beside a GitHub one.
rn="$(mktemp -d)"
(
  set -e
  export XDG_CONFIG_HOME="${rn}/config"
  git init -q --bare -b main "${rn}/remote/p-tickets.git"
  git clone -q "${rn}/remote/p-tickets.git" "${rn}/work/p-tickets" 2>/dev/null
  cd "${rn}/work/p-tickets"
  git config user.email t@t.co && git config user.name t && git config commit.gpgsign false
  "${AMD}" init p --local-only >/dev/null 2>&1
  git add -A && git commit -q -m "seed for ticket 228" && git push -q origin HEAD:main 2>/dev/null
  git branch -q --set-upstream-to=origin/main
  "${AMD}" new "Take card payments" --no-edit >/dev/null 2>&1
  # One that already exists on the remote, not cloned here.
  git init -q --bare -b main "${rn}/remote/p-existing.git"
  git clone -q "${rn}/remote/p-existing.git" "${rn}/seed" 2>/dev/null
  git -C "${rn}/seed" -c user.email=t@t.co -c user.name=t commit -q --allow-empty -m "already here"
  git -C "${rn}/seed" push -q origin HEAD:main 2>/dev/null
)
in_rn() { (cd "${rn}/work/p-tickets" && env XDG_CONFIG_HOME="${rn}/config" RITE_NO_BRANCH=1 "${AMD}" "$@"); }
assert "rite repo new --for makes it beside the remote, clones it beside the tickets repository, with a README" \
  bash -c "$(declare -f in_rn); rn='${rn}'; AMD='${AMD}'; in_rn repo new p-payments --for 1 2>/dev/null \
    | grep -q \"created p-payments at ${rn}/work/p-payments\" \
    && [ -d '${rn}/remote/p-payments.git' ] && [ -f '${rn}/work/p-payments/README.md' ] \
    && grep -q 'ticket 001, Take card payments' '${rn}/work/p-payments/README.md' \
    && [ \"\$(git -C '${rn}/remote/p-payments.git' log -1 --format=%s main)\" = 'Start the repository' ]"
assert "...declares and registers it, and the ticket says so, in one commit" \
  bash -c "rn='${rn}'; grep -q '^repos = .*p-payments' '${rn}/work/p-tickets/.settings' \
    && grep -q '/work/p-payments\$' '${rn}/config/kanrite/repos' \
    && grep -q '^repository: \"p-payments\"' '${rn}'/work/p-tickets/tasks/backlog/001-*.md \
    && grep -q 'Created repository p-payments (private).' '${rn}'/work/p-tickets/tasks/backlog/001-*.md \
    && [ \"\$(git -C '${rn}/work/p-tickets' log -1 --format=%s)\" = 'repo: new \"p-payments\"' ] \
    && git -C '${rn}/work/p-tickets' show --stat --format= HEAD | grep -q '.settings' \
    && git -C '${rn}/work/p-tickets' show --stat --format= HEAD | grep -q '001-'"
assert "making it again is refused" \
  bash -c "$(declare -f in_rn); rn='${rn}'; AMD='${AMD}'; in_rn repo new p-payments 2>&1 | grep -q 'p-payments is already cloned'"
assert "a repository already on the remote is connected, not made again" \
  bash -c "$(declare -f in_rn); rn='${rn}'; AMD='${AMD}'; in_rn repo new p-existing 2>/dev/null | grep -q '^connected p-existing' \
    && [ \"\$(git -C '${rn}/work/p-existing' log -1 --format=%s)\" = 'already here' ] \
    && [ ! -f '${rn}/work/p-existing/README.md' ]"
assert "a name with a slash or a space is refused" \
  bash -c "$(declare -f in_rn); rn='${rn}'; AMD='${AMD}'; ! in_rn repo new 'bad name' >/dev/null 2>&1 && ! in_rn repo new a/b >/dev/null 2>&1"
rm -rf "${rn}"

echo "board settings live in the tickets repository (ticket 111):"
# A project whose documents repository still holds settings from before
# 111: repos and wip-limit there, an older repos value in tickets.
st="$(mktemp -d)"
st_env="XDG_CONFIG_HOME=${st}/config"
(
  set -e
  export XDG_CONFIG_HOME="${st}/config"
  cd "${st}"
  git init -q -b main proj-tickets && git init -q -b main proj-documents && git init -q -b main code
  for r in proj-tickets proj-documents code; do
    git -C "${r}" config user.email t@t.co && git -C "${r}" config user.name t && git -C "${r}" config commit.gpgsign false
  done
  cd proj-tickets
  RITE_COMMIT=0 "${AMD}" init proj --local-only >/dev/null 2>&1
  echo "repos = old-repo" >>.settings
  git add -A && git commit -q -m "seed for ticket 111"
  mkdir -p ../proj-documents/.settings
  echo "old-repo,docs-repo" >../proj-documents/.settings/repos
  echo "4" >../proj-documents/.settings/wip-limit
)
assert "reading the board carries nothing across: no migration on open (ticket 193)" \
  bash -c "cd '${st}/proj-tickets' && ! env ${st_env} '${AMD}' board --plain 2>&1 | grep -q 'moved ' \
    && ! grep -q 'docs-repo' .settings && [ -z \"\$(git status --porcelain)\" ]"
assert "the first change carries documents settings across, and says so" \
  bash -c "cd '${st}/proj-tickets' && out=\"\$(env ${st_env} '${AMD}' new 'First change' --no-edit 2>&1)\" \
    && grep -q '^moved .*repos.* from .*proj-documents/.settings' <<<\"\${out}\" && grep -q '^moved .*wip-limit' <<<\"\${out}\""
assert "...repos is the union, wip-limit filled in, in the tickets .settings" \
  bash -c "grep -qx 'repos = old-repo,docs-repo' '${st}/proj-tickets/.settings' && grep -qx 'wip-limit = 4' '${st}/proj-tickets/.settings'"
assert "...committed, like any change" \
  bash -c "git -C '${st}/proj-tickets' log -2 --format=%s | grep -q '^settings: migrate'"
assert "...and silent once everything is across" \
  bash -c "cd '${st}/proj-tickets' && ! env ${st_env} '${AMD}' board --plain 2>&1 | grep -q 'moved '"
assert "...once only: a repository deregistered stays off, though documents still names it (ticket 182)" \
  bash -c "cd '${st}/proj-tickets' && grep -qx 'migrated-from-documents = on' .settings \
    && env ${st_env} '${AMD}' deregister docs-repo >/dev/null 2>&1 \
    && env ${st_env} '${AMD}' board --plain >/dev/null 2>&1 \
    && grep -qx 'repos = old-repo' .settings"
assert "rite wip-limit is committed" \
  bash -c "cd '${st}/proj-tickets' && env ${st_env} '${AMD}' wip-limit 3 >/dev/null 2>&1 \
    && grep -qx 'wip-limit = 3' .settings && git log -1 --format=%s | grep -q '^settings: wip-limit'"
assert "rite shared-documents names the shared documents repository, committed (ticket 129)" \
  bash -c "cd '${st}/proj-tickets' && env ${st_env} '${AMD}' shared-documents org-documents >/dev/null 2>&1 \
    && grep -qx 'shared-documents = org-documents' .settings \
    && git log -1 --format=%s | grep -q '^settings: shared-documents' \
    && env ${st_env} '${AMD}' shared-documents 2>/dev/null | grep -qx org-documents"
assert "...and --clear removes it" \
  bash -c "cd '${st}/proj-tickets' && env ${st_env} '${AMD}' shared-documents --clear >/dev/null 2>&1 \
    && ! grep -q '^shared-documents' .settings"
assert "rite display-name sets the name shown, committed, keeping the project's own (ticket 326)" \
  bash -c "cd '${st}/proj-tickets' && env ${st_env} '${AMD}' display-name 'Proj Co' >/dev/null 2>&1 \
    && grep -qx 'display-name = Proj Co' .settings && grep -q '^project = ' .settings \
    && git log -1 --format=%s | grep -q '^settings: display-name' \
    && env ${st_env} '${AMD}' display-name 2>/dev/null | grep -qx 'Proj Co'"
assert "...rite project lists it beside the name, and project use still takes the name" \
  bash -c "cd '${st}' && env ${st_env} '${AMD}' project 2>/dev/null | grep -q '^. proj .*(Proj Co)' \
    && env ${st_env} '${AMD}' project use proj >/dev/null 2>&1"
assert "...a name over 60 characters is refused, changing nothing" \
  bash -c "cd '${st}/proj-tickets' && ! env ${st_env} '${AMD}' display-name \"$(printf 'x%.0s' $(seq 61))\" >/dev/null 2>&1 \
    && grep -qx 'display-name = Proj Co' .settings"
assert "...and --clear shows the project's own name again" \
  bash -c "cd '${st}/proj-tickets' && env ${st_env} '${AMD}' display-name --clear >/dev/null 2>&1 \
    && ! grep -q '^display-name' .settings && ! env ${st_env} '${AMD}' project 2>/dev/null | grep -q '(Proj Co)'"
assert "rite register declares the repository in the tickets .settings, committed" \
  bash -c "cd '${st}/proj-tickets' && env ${st_env} '${AMD}' register '${st}/code' >/dev/null 2>&1 \
    && grep -q '^repos = .*code' .settings && git log -1 --format=%s | grep -qx 'register: code'"
assert "rite register --remove undeclares it for everyone, committed" \
  bash -c "cd '${st}/proj-tickets' && env ${st_env} '${AMD}' register --remove code 2>&1 | grep -q 'no longer part of the project' \
    && ! grep -q '^repos = .*code' .settings && git log -1 --format=%s | grep -qx 'unregister: code' && test -d '${st}/code'"
assert "...and refuses a repository that isn't declared" \
  bash -c "cd '${st}/proj-tickets' && env ${st_env} '${AMD}' register --remove nowhere 2>&1 | grep -q \"nowhere isn't declared\""
rm -rf "${st}"

echo "create:"
# Tags have to be listed before use (ticket 167): only DRAFT and TRIAGE
# are there to begin with.
"${AMD}" tags add x >/dev/null && "${AMD}" tags add y >/dev/null
"${AMD}" new "First task" >/dev/null
"${AMD}" new "Second task" -t x -t y >/dev/null
git add -A; git commit -qm seed
assert "creates tasks/backlog/001-first-task.md (new work starts in the backlog)" test -f tasks/backlog/001-first-task.md
assert "title in frontmatter" grep -q '^title: "First task"$' tasks/backlog/001-first-task.md
assert "tags in frontmatter, in UPPERCASE (ticket 122)" grep -q '^tags: \[X,Y\]$' tasks/backlog/002-second-task.md
assert "a new ticket has priority low" grep -q '^priority: "low"$' tasks/backlog/002-second-task.md
assert "rite set priority changes it" \
  bash -c "'${AMD}' set 2 priority high | grep -q 'priority high' && grep -q '^priority: \"high\"\$' tasks/backlog/002-second-task.md"
assert "an invalid priority is refused" \
  bash -c "'${AMD}' set 2 priority urgent 2>&1 | grep -q 'low, medium, high' && grep -q '^priority: \"high\"\$' tasks/backlog/002-second-task.md"
assert "a new ticket has no branch type by default" \
  grep -q '^branch-type: ""$' tasks/backlog/001-first-task.md
assert "and so has no branch name" grep -q '^branch-name: ""$' tasks/backlog/001-first-task.md
assert "a new ticket is unassigned" grep -q '^assignee: ""$' tasks/backlog/001-first-task.md
assert "created date is filled in" grep -qE '^created: "[0-9]{4}-[0-9]{2}-[0-9]{2}"$' tasks/backlog/001-first-task.md

echo "parents and the tree:"
"${AMD}" new "Guest checkout" --branch-type bugfix >/dev/null
"${AMD}" new "Address form" --parent 3 >/dev/null
assert "the child records its parent id" grep -q '^parent: "003"$' tasks/backlog/004-address-form.md
assert "the child links back to the parent" \
  bash -c "grep -q '^\[\[003-guest-checkout\]\]$' tasks/backlog/004-address-form.md"
assert "the parent lists the child" \
  bash -c "grep -q '^- \[\[004-address-form\]\]$' tasks/backlog/003-guest-checkout.md"
assert "a ticket with no parent has an empty parent field" \
  bash -c "grep -q '^parent: \"\"$' tasks/backlog/001-first-task.md"
assert "an unknown parent is rejected" bash -c "! '${AMD}' new 'Orphan' --parent 99"
assert "the board nests a child under its parent" \
  bash -c "'${AMD}' board --plain | grep -q '^    \[004\] Address form'"
assert "the parent stays at the outer level" \
  bash -c "'${AMD}' board --plain | grep -q '^  \[003\] Guest checkout'"
"${AMD}" new "Deep child" --parent 4 >/dev/null
assert "nesting goes deeper than two levels" \
  bash -c "'${AMD}' board --plain | grep -q '^      \[005\] Deep child'"
assert "epics and stories are gone" \
  bash -c "! '${AMD}' epics 2>/dev/null && ! '${AMD}' stories 2>/dev/null"

echo "board:"
assert "board shows all three columns" \
  bash -c "'${AMD}' board | grep -q TODO && '${AMD}' board | grep -q DOING && '${AMD}' board | grep -q DONE"
assert "board lists the task by id and title" \
  bash -c "'${AMD}' board | grep -q '\[001\] First task'"
assert "empty columns say so" bash -c "'${AMD}' doing | grep -q '(empty)'"

echo "branch types:"
"${AMD}" new "Crash on save" --branch-type bugfix >/dev/null
assert "the branch type is recorded" grep -q '^branch-type: "bugfix"$' tasks/backlog/006-crash-on-save.md
assert "the branch name comes from the type and the title" \
  grep -q '^branch-name: "bugfix/crash-on-save"$' tasks/backlog/006-crash-on-save.md
assert "an unknown branch type is rejected, listing the valid ones" \
  bash -c "'${AMD}' new 'Nope' --branch-type feat 2>&1 | grep -q \"unknown branch type 'feat'\""
assert "RITE_BRANCH_TYPES overrides the list" \
  bash -c "RITE_BRANCH_TYPES='spike,chore' '${AMD}' new 'Try it' --branch-type spike >/dev/null && grep -q '^branch-name: \"spike/try-it\"\$' tasks/backlog/007-try-it.md"
assert "there is one ticket template now" \
  bash -c "'${AMD}' templates | grep -q '^ticket' && ! '${AMD}' templates | grep -q '^admin'"
assert "the board shows the branch type" \
  bash -c "'${AMD}' board --plain | grep -q 'Crash on save  (bugfix)'"

echo "moves (git mv) + branches — entering todo/ is what creates one now:"
git add -A; git commit -qm labels
"${AMD}" todo 1 >/dev/null
assert "a ticket with no branch type creates no branch" \
  bash -c "! git branch --list 'feature/first-task' | grep -q ."
assert "it still moves to todo" test -f tasks/todo/001-first-task.md
assert "todo says why it left the branch alone" \
  bash -c "'${AMD}' backlog 1 >/dev/null && '${AMD}' todo 1 2>&1 | grep -q 'no branch type on this ticket'"
git checkout -q main 2>/dev/null || git checkout -q master
"${AMD}" todo 6 >/dev/null
assert "a ticket with a branch type gets its branch" \
  bash -c "git branch --list 'bugfix/crash-on-save' | grep -q ."
"${AMD}" --no-input todo 3 --no-branch >/dev/null
assert "--no-branch leaves the branch alone" \
  bash -c "! git branch --list 'bugfix/guest-checkout' | grep -q ."
assert "todo: no-branch still moved the task" test -f tasks/todo/003-guest-checkout.md
"${AMD}" backlog 3 >/dev/null
"${AMD}" todo 3 --branch spike/try-something >/dev/null
assert "--branch overrides the ticket" \
  bash -c "git branch --show-current | grep -q '^spike/try-something$'"
assert "an invalid --branch is rejected" \
  bash -c "'${AMD}' todo 5 --branch 'bad branch' 2>&1 | grep -q 'cannot contain whitespace'"
git checkout -q main 2>/dev/null || git checkout -q master
assert "doing is a plain move now: no branch effect at all" \
  bash -c "'${AMD}' doing 1 >/dev/null && test -f tasks/doing/001-first-task.md"
RITE_COMMIT=0 "${AMD}" "done" 1 >/dev/null
assert "done: doing -> done" test -f tasks/done/001-first-task.md
assert "moved via git (rename tracked)" bash -c 'git status --porcelain | grep -q "^R"'
# The move message comes from the CLI now, not from the library (ADR-0004).
assert "a move reports where the ticket went" \
  bash -c "'${AMD}' todo 2 --no-branch | grep -q 'moved 002-second-task.md -> todo/'"
assert "a ticket can't skip a column" \
  bash -c "'${AMD}' done 2 2>&1 | grep -q 'one column at a time' && test -f tasks/todo/002-second-task.md"
assert "moving into the same column fails" bash -c "! '${AMD}' done 1"
assert "rite start no longer exists" bash -c "! '${AMD}' start 1 2>/dev/null"

echo "destination verbs (move + list, ticket 002):"
assert "rite todo <ref> moves it to todo/" \
  bash -c "'${AMD}' todo 4 >/dev/null && test -f tasks/todo/004-address-form.md"
assert "rite doing <ref> moves it to doing/" \
  bash -c "'${AMD}' doing 4 >/dev/null && test -f tasks/doing/004-address-form.md"
assert "rite backlog <ref> moves it back to backlog/, a column at a time" \
  bash -c "'${AMD}' todo 4 --no-branch >/dev/null && '${AMD}' backlog 4 >/dev/null && test -f tasks/backlog/004-address-form.md"
assert "rite ls is gone: the bare destination verbs list (ticket 050)" \
  bash -c "! '${AMD}' ls >/dev/null 2>&1 && ! '${AMD}' list >/dev/null 2>&1"
assert "an unknown destination names what is valid" \
  bash -c "'${AMD}' retest-tui 4 2>&1 | grep -q \"'retest-tui' is not a column, 'abandon', or an epic on this board\""
assert "the error lists the columns and says there are no epics" \
  bash -c "'${AMD}' retest-tui 4 2>&1 | grep -q 'columns: backlog, todo, doing, done; epics: none'"
assert "rite back no longer exists" bash -c "! '${AMD}' back 4 2>/dev/null"

echo "refs + ids:"
"${AMD}" todo second >/dev/null
assert "find by slug substring" test -f tasks/todo/002-second-task.md
git checkout -q main 2>/dev/null || git checkout -q master
"${AMD}" new "Third" >/dev/null
assert "ids continue across columns (008)" test -f tasks/backlog/008-third.md
assert "unknown ref fails" bash -c "! '${AMD}' show 99"
assert "ambiguous ref fails" bash -c "! '${AMD}' show task"
assert "show prints the file" bash -c "'${AMD}' show 008 | grep -q '^title: \"Third\"$'"

echo "templates:"
assert "templates says where each ticket type comes from" \
  bash -c "'${AMD}' templates | grep -q '^ticket  *built-in$'"
assert "unknown template fails with a hint" \
  bash -c "'${AMD}' new 'Nope' --template nope 2>&1 | grep -q 'rite templates'"
assert "eject writes an editable copy into the board" \
  bash -c "'${AMD}' templates eject ticket >/dev/null && test -f tasks/templates/ticket.md.jinja"
assert "eject refuses to clobber without --force" \
  bash -c "! '${AMD}' templates eject ticket"
printf -- '---\nid: {{ id | yaml }}\ntitle: {{ title | yaml }}\ntype: {{ branch_type | yaml }}\nassignee: {{ extra.owner | yaml }}\n---\n\n## Custom\n' \
  > tasks/templates/ticket.md.jinja
assert "board template overrides the built-in" \
  bash -c "'${AMD}' new 'Overridden' -s owner=tim >/dev/null && grep -q '^## Custom$' tasks/backlog/*-overridden.md"
assert "--set values reach the template" \
  bash -c "grep -q '^assignee: \"tim\"$' tasks/backlog/*-overridden.md"
assert "labels reach a custom template" \
  bash -c "grep -q '^type: \"\"$' tasks/backlog/*-overridden.md"
assert "templates list shows the board override" \
  bash -c "'${AMD}' templates | grep -q 'templates/ticket.md.jinja'"
printf -- '---\nid: {{ id | yaml }}\nassignee: {{ extra.owner | yaml }}\ndue: {{ extra["due date"] | yaml }}\n---\n' \
  > tasks/templates/ticket.md.jinja
assert "a template field with no value is an error, naming the flag" \
  bash -c "'${AMD}' new 'Needs fields' 2>&1 | grep -q -- '--set owner='"
assert "every missing field is listed" \
  bash -c "'${AMD}' new 'Needs fields' 2>&1 | grep -q -- '--set due date='"
printf 'title: {{ titel }}\n' > tasks/templates/ticket.md.jinja
assert "a typo'd variable is an error, not a blank line" \
  bash -c "! '${AMD}' new 'Broken' 2>&1 | grep -q '^title: $'"
assert "the template error names the template and line" \
  bash -c "'${AMD}' new 'Broken' 2>&1 | grep -q \"template 'ticket'\""
rm -rf tasks/templates

echo "the body editor:"
assert "non-interactive create never opens an editor" \
  bash -c "EDITOR=false '${AMD}' new 'No editor here' >/dev/null && test -f tasks/backlog/*-no-editor-here.md"
assert "--no-edit is accepted alongside a title" \
  bash -c "'${AMD}' new 'Also no editor' --no-edit >/dev/null"
assert "--edit and --no-edit conflict" \
  bash -c "! '${AMD}' new 'Both' --edit --no-edit"

echo "quoting:"
"${AMD}" new 'Fix the "quoted" thing' >/dev/null
assert "a quote in the title is escaped, not left to break the frontmatter" \
  bash -c "grep -qF 'title: \"Fix the \\\"quoted\\\" thing\"' tasks/backlog/*-fix-the-quoted-thing.md"

echo "the archive:"
assert "init creates an archive that keeps itself out of git" \
  bash -c "test -f tasks/archive/.gitignore && grep -q '^\*$' tasks/archive/.gitignore"
"${AMD}" new "Bin this one" >/dev/null
git add -A; git commit -qm junkable
assert "abandon takes a ticket off the board" \
  bash -c "RITE_COMMIT=0 '${AMD}' abandon bin-this-one | grep -q 'archived'"
assert "the ticket is in the archive" test -f tasks/archive/*-bin-this-one.md
assert "rite abandon with no ref lists the archive" \
  bash -c "'${AMD}' abandon | grep -q 'Bin this one'"
assert "the board no longer shows it" \
  bash -c "! '${AMD}' board --plain | grep -q 'Bin this one'"
assert "git records the ticket leaving the board" \
  bash -c "git status --porcelain | grep -q '^D  tasks/backlog/.*bin-this-one'"
assert "the archive stays out of git" bash -c "! git status --porcelain | grep -q 'tasks/archive/0'"
assert "an archived ticket is no longer findable" bash -c "! '${AMD}' show bin-this-one"
assert "ids are not reused after archiving" \
  bash -c "before=\$(ls tasks/archive | head -1 | cut -d- -f1); '${AMD}' new 'After the bin' >/dev/null; ! test -f tasks/backlog/\${before}-after-the-bin.md"
assert "the counter records the next id in its own file, not .settings or tasks/ (ticket 150)" \
  bash -c "test ! -f tasks/.next-id && ! grep -q '^next-id' .settings \
    && test \"\$(cat .next-id)\" -gt \"\$(ls tasks/backlog | tail -1 | cut -d- -f1 | sed 's/^0*//')\""
assert "a deleted ticket does not hand its id back" \
  bash -c "'${AMD}' new 'Doomed' >/dev/null; id=\$(ls tasks/backlog | grep doomed | cut -d- -f1); rm tasks/backlog/\${id}-doomed.md; '${AMD}' new 'After the delete' >/dev/null; ! test -f tasks/backlog/\${id}-after-the-delete.md"

echo "branch cleanup on abandon:"
"${AMD}" new "Cleanup candidate" --branch-type feature >/dev/null
cid=$(basename tasks/backlog/*-cleanup-candidate.md | cut -d- -f1)
"${AMD}" todo "${cid}" >/dev/null
assert "the branch exists once the ticket enters todo" \
  bash -c "git branch --list 'feature/cleanup-candidate' | grep -q ."
git checkout -q main 2>/dev/null || git checkout -q master
assert "abandoning an untouched branch deletes it too" \
  bash -c "'${AMD}' abandon ${cid} | grep -q 'deleted feature/cleanup-candidate'"
assert "the branch is actually gone" \
  bash -c "! git branch --list 'feature/cleanup-candidate' | grep -q ."

"${AMD}" new "Cleanup keeps real work" --branch-type feature >/dev/null
kid=$(basename tasks/backlog/*-cleanup-keeps-real-work.md | cut -d- -f1)
# Commit the ticket to the base branch first: otherwise it only exists on the
# feature branch's own history, and checking back out to main makes it vanish
# from the working tree (same trap the board.rs unit test hit).
git add -A; git commit -qm "add cleanup-keeps-real-work ticket"
"${AMD}" todo "${kid}" >/dev/null
echo "real work" > realwork.txt
git add -A; git commit -qm "real work on the branch"
git checkout -q main 2>/dev/null || git checkout -q master
assert "abandoning a branch with real commits leaves it alone" \
  bash -c "out=\$('${AMD}' abandon ${kid}); echo \"\$out\" | grep -q archived && ! echo \"\$out\" | grep -q deleted"
assert "the branch with real work survives" \
  bash -c "git branch --list 'feature/cleanup-keeps-real-work' | grep -q ."

echo "assignees:"
assert "a new ticket is unassigned" \
  bash -c "grep -q '^assignee: \"\"$' tasks/*/001-first-task.md"
"${AMD}" new "Assigned at creation" -a alex >/dev/null
assert "--assignee sets it at creation" \
  bash -c "grep -q '^assignee: \"alex\"$' tasks/backlog/*-assigned-at-creation.md"
assert "rite assign sets it afterwards" \
  bash -c "'${AMD}' assign 1 sam >/dev/null && grep -q '^assignee: \"sam\"$' tasks/*/001-first-task.md"
assert "rite assign with no name clears it" \
  bash -c "'${AMD}' assign 1 >/dev/null && grep -q '^assignee: \"\"$' tasks/*/001-first-task.md"
assert "@me resolves to the git user" \
  bash -c "'${AMD}' assign 1 @me >/dev/null && grep -q '^assignee: \"t\"$' tasks/*/001-first-task.md"
assert "the board shows the assignee" bash -c "'${AMD}' board --plain | grep -q '@t'"
# Ticket 254: several people. One is written as before; two or more keep
# `assignee` as the first and add an `assignees` list.
assert "rite assign with two names assigns both, the first as assignee" \
  bash -c "'${AMD}' assign 1 sam jo >/dev/null && grep -q '^assignee: \"sam\"$' tasks/*/001-first-task.md \
    && grep -q '^assignees: \[\"sam\",\"jo\"\]$' tasks/*/001-first-task.md"
assert "the board shows every assignee" bash -c "'${AMD}' board --plain | grep -q '@sam @jo'"
assert "rite assign --add adds after them" \
  bash -c "'${AMD}' assign 1 --add ann | grep -q 'assigned to sam, jo, ann' \
    && grep -q '^assignees: \[\"sam\",\"jo\",\"ann\"\]$' tasks/*/001-first-task.md"
assert "rite assign --remove takes the first off and the next leads" \
  bash -c "'${AMD}' assign 1 --remove SAM >/dev/null && grep -q '^assignee: \"jo\"$' tasks/*/001-first-task.md \
    && grep -q '^assignees: \[\"jo\",\"ann\"\]$' tasks/*/001-first-task.md"
assert "down to one person, the list goes" \
  bash -c "'${AMD}' assign 1 --remove ann >/dev/null && grep -q '^assignee: \"jo\"$' tasks/*/001-first-task.md \
    && ! grep -q '^assignees:' tasks/*/001-first-task.md"
"${AMD}" new "Shared work" -a @me -a jo --no-edit >/dev/null
shared="$(ls tasks/backlog/*-shared-work.md)"
assert "rite new -a twice assigns both" \
  bash -c "grep -q '^assignee: \"t\"$' '${shared}' && grep -q '^assignees: \[\"t\",\"jo\"\]$' '${shared}'"
# Someone else's lease: every assignee may still write the ticket.
sed -i 's/^assignee: "t"$/assignee: "t"\nlocked-by: "jo@elsewhere.co"\nlocked-until: "2999-01-01T00:00:00Z"/' "${shared}"
assert "an assignee can write a ticket someone else has locked" \
  bash -c "'${AMD}' set \$(basename '${shared}' | cut -c1-3) points 3 >/dev/null 2>&1 && grep -q '^points: \"3\"$' '${shared}'"
sed -i 's/^assignee: "t"$/assignee: "jo"/; s/^assignees: .*$/assignees: ["jo"]/' "${shared}"
assert "...and anyone else can't" \
  bash -c "! '${AMD}' set \$(basename '${shared}' | cut -c1-3) points 5 >/dev/null 2>&1 && grep -q '^points: \"3\"$' '${shared}'"
sed -i '/^locked-by:/d; /^locked-until:/d' "${shared}"
# Ticket 089: the users list is what assignees are picked from; anyone
# else is allowed, with a warning, and a board with no list never warns.
as_list="$(mktemp -d)"
( cd "${as_list}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_COMMIT=0 "${AMD}" init listed --local-only >/dev/null 2>&1 \
  && RITE_COMMIT=0 "${AMD}" new "Who does it" --no-edit >/dev/null 2>&1 )
in_list() { (cd "${as_list}" && RITE_COMMIT=0 "${AMD}" "$@"); }
assert "assigning one of the project's users doesn't warn" \
  bash -c "$(declare -f in_list); as_list='${as_list}'; AMD='${AMD}'; [ -z \"\$(in_list assign 1 t 2>&1 >/dev/null)\" ] \
    && grep -q '^assignee: \"t\"$' '${as_list}'/tasks/backlog/001-who-does-it.md"
assert "assigning someone else works, and warns they aren't a user" \
  bash -c "$(declare -f in_list); as_list='${as_list}'; AMD='${AMD}'; in_list assign 1 stranger 2>&1 >/dev/null \
    | grep -q \"warning: stranger isn't one of this project's users — .rite join. adds them\" \
    && grep -q '^assignee: \"stranger\"$' '${as_list}'/tasks/backlog/001-who-does-it.md"
assert "...and so does rite new --assignee" \
  bash -c "$(declare -f in_list); as_list='${as_list}'; AMD='${AMD}'; in_list new 'For a stranger' --assignee stranger --no-edit 2>&1 >/dev/null \
    | grep -q \"stranger isn't one of this project's users\""
assert "rite assign --clear unassigns" \
  bash -c "$(declare -f in_list); as_list='${as_list}'; AMD='${AMD}'; in_list assign 1 --clear | grep -q '001 unassigned' \
    && grep -q '^assignee: \"\"$' '${as_list}'/tasks/backlog/001-who-does-it.md"
assert "a board with no users list never warns" \
  bash -c "sed -i '/^users = /d' '${as_list}/.settings' \
    && [ -z \"\$(cd '${as_list}' && RITE_COMMIT=0 '${AMD}' assign 1 anyone 2>&1 >/dev/null)\" ]"
rm -rf "${as_list}"

echo "registration:"
tmp_name="$(basename "${tmp}")"
# periods_and_underscores was deregistered earlier (right after its own
# init --local-only) — reused here specifically because it's a real board
# that's confirmed *not* currently registered, to check that merely running
# a board-resolving command doesn't silently re-add it.
assert "rite board no longer registers anything on its own" \
  bash -c "cd '${periods_and_underscores}' && '${AMD}' board >/dev/null; \
    ! grep -q \"$(basename "${periods_and_underscores}")\$\" \"\${XDG_CONFIG_HOME}/kanrite/repos\" 2>/dev/null"
assert "rite register declares the repo for the project and registers it locally" \
  bash -c "'${AMD}' register | grep -q 'registered' \
    && grep -qE \"^repos = .*${tmp_name}\" .settings \
    && grep -q \"${tmp_name}\$\" \"\${XDG_CONFIG_HOME}/kanrite/repos\""
assert "registering the same path again is a local no-op, not an error" \
  bash -c "'${AMD}' register | grep -q 'already registered on this machine'"
assert "a non-repository is refused" \
  bash -c "'${AMD}' register '${nongit}' 2>&1 | grep -q 'not a git repository'"
assert "rite deregister takes a declared repository off the project and this machine (ticket 120)" \
  bash -c "'${AMD}' deregister \"${tmp_name}\" | grep -q 'no longer part of the project' \
    && ! grep -qE \"^repos = .*${tmp_name}\" .settings \
    && ! grep -q \"${tmp_name}\$\" \"\${XDG_CONFIG_HOME}/kanrite/repos\""
assert "deregistering what's neither declared nor registered fails" \
  bash -c "! '${AMD}' deregister nope"

echo "aliases (removed, ticket 097):"
assert "rite alias is no longer a command" \
  bash -c "! '${AMD}' alias somerepo somename >/dev/null 2>&1"

echo "related tickets:"
assert "related is empty by default" \
  bash -c "grep -q '^related: \[\]$' tasks/*/001-first-task.md"
"${AMD}" new "Depends on the first" --related 1 >/dev/null
assert "--related records the id" \
  bash -c "grep -q '^related: \[001\]$' tasks/backlog/*-depends-on-the-first.md"
assert "the other end is linked back" \
  bash -c "grep -qE '^related: \[0[0-9]+\]$' tasks/*/001-first-task.md"
assert "an unknown related ref is rejected at creation" \
  bash -c "! '${AMD}' new 'Dangling' --related 99"
assert "rite link relates two tickets both ways" \
  bash -c "'${AMD}' link 2 depends-on-the-first >/dev/null \
    && grep -q 'related: \[0' tasks/*/002-second-task.md \
    && grep -q '002' tasks/backlog/*-depends-on-the-first.md"
assert "rite link is idempotent" \
  bash -c "'${AMD}' link 2 depends-on-the-first | grep -q 'already relates to'"
assert "a task cannot be related to itself" \
  bash -c "'${AMD}' link 2 2 2>&1 | grep -q \"can't be linked to itself\""
"${AMD}" new "One way only" >/dev/null
assert "rite link --one-way leaves the other end alone" \
  bash -c "'${AMD}' link 2 one-way --one-way >/dev/null && ! grep -q '002' tasks/backlog/*-one-way-only.md"
assert "a ticket with no branch carries the list too" \
  bash -c "grep -q '^related: \[\]$' tasks/*/*-assigned-at-creation.md"

echo "typed links (ticket 207):"
"${AMD}" new "Link blocker" >/dev/null
"${AMD}" new "Link blocked" >/dev/null
assert "rite link --type requires records the opposite on the other ticket" \
  bash -c "'${AMD}' link link-blocker link-blocked --type requires >/dev/null \
    && grep -q '^links: \[requires:0' tasks/backlog/*-link-blocker.md \
    && grep -q '^links: \[required-by:0' tasks/backlog/*-link-blocked.md"
assert "a typed link is idempotent" \
  bash -c "'${AMD}' link link-blocker link-blocked --type requires | grep -q 'already requires'"
assert "a ticket may take several links of different types" \
  bash -c "'${AMD}' link link-blocker link-blocked --type supersedes >/dev/null \
    && grep -q '^links: \[requires:0[0-9]*,supersedes:0' tasks/backlog/*-link-blocker.md"
assert "an unknown link type is refused" \
  bash -c "! '${AMD}' link link-blocker link-blocked --type befriends 2>/dev/null"
assert "rite link --remove takes it away from both tickets" \
  bash -c "'${AMD}' link link-blocker link-blocked --type requires --remove >/dev/null \
    && ! grep -q 'requires:' tasks/backlog/*-link-blocker.md \
    && ! grep -q 'required-by:' tasks/backlog/*-link-blocked.md"
assert "child-of sets the parent and lists the child" \
  bash -c "'${AMD}' link link-blocked link-blocker --type child-of >/dev/null \
    && grep -q '^parent: \"0' tasks/backlog/*-link-blocked.md \
    && grep -q 'link-blocked' tasks/backlog/*-link-blocker.md"

echo "done waits for required tickets and children (ticket 213):"
dw="$(mktemp -d)"
( cd "${dw}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init waits --local-only >/dev/null \
  && for t in Feature Dependency Child; do "${AMD}" new "$t" >/dev/null; done \
  && "${AMD}" link 1 2 --type requires >/dev/null && "${AMD}" link 3 1 --type child-of >/dev/null \
  && RITE_NO_BRANCH=1 "${AMD}" todo 1 >/dev/null && "${AMD}" doing 1 >/dev/null )
in_dw() { (cd "${dw}" && RITE_NO_BRANCH=1 "${AMD}" "$@"); }
assert "done is refused while a required ticket and a child aren't done, naming them" \
  bash -c "$(declare -f in_dw); dw='${dw}'; AMD='${AMD}'; out=\$(in_dw done 1 2>&1); [ \$? -ne 0 ] \
    && printf '%s' \"\$out\" | grep -q '002 (required, in backlog), 003 (a child, in backlog) aren.t done' \
    && test -f '${dw}'/tasks/doing/001-feature.md"
assert "rite link --replace changes a link's type on both tickets" \
  bash -c "$(declare -f in_dw); dw='${dw}'; AMD='${AMD}'; in_dw link 1 2 --replace requires --type relates-to >/dev/null \
    && grep -q '^related: \[002\]' '${dw}'/tasks/doing/001-feature.md \
    && grep -q '^related: \[001\]' '${dw}'/tasks/backlog/002-dependency.md \
    && ! grep -q 'requires:' '${dw}'/tasks/doing/001-feature.md \
    && git -C '${dw}' log --format=%s -1 | grep -q '^relink: 001 -> 002'"
assert "--replace refuses a link that isn't there" \
  bash -c "$(declare -f in_dw); dw='${dw}'; AMD='${AMD}'; ! in_dw link 1 2 --replace supersedes --type relates-to 2>/dev/null"
assert "with the child done, the parent can be done" \
  bash -c "$(declare -f in_dw); dw='${dw}'; AMD='${AMD}'; in_dw done 1 2>&1 | grep -q '003 (a child' \
    && in_dw todo 3 >/dev/null && in_dw doing 3 >/dev/null && in_dw done 3 >/dev/null \
    && in_dw done 1 >/dev/null && test -f '${dw}'/tasks/done/001-feature.md"
rm -rf "${dw}"

echo "requires and the sprint start check (ticket 214):"
rq="$(mktemp -d)"
( cd "${rq}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init reqs --local-only >/dev/null \
  && "${AMD}" new "Base" >/dev/null && "${AMD}" new "Other" >/dev/null \
  && "${AMD}" new "Top" --requires 1 --requires other >/dev/null )
in_rq() { (cd "${rq}" && RITE_NO_BRANCH=1 "${AMD}" "$@"); }
assert "rite new --requires links both ends in the new ticket's own commit" \
  bash -c "grep -q '^links: \[requires:001,requires:002\]' '${rq}'/tasks/backlog/003-top.md \
    && grep -q '^links: \[required-by:003\]' '${rq}'/tasks/backlog/001-base.md \
    && [ \"\$(git -C '${rq}' log --format=%s -1)\" = 'new: 003 \"Top\"' ] \
    && git -C '${rq}' show --stat HEAD | grep -q '001-base.md'"
assert "rite new --requires with an unknown ticket creates nothing" \
  bash -c "$(declare -f in_rq); rq='${rq}'; AMD='${AMD}'; ! in_rq new 'Nope' --requires 99 2>/dev/null \
    && ! ls '${rq}'/tasks/backlog/*-nope.md 2>/dev/null"
assert "rite set requires makes the list exactly that, both ends, and empty clears" \
  bash -c "$(declare -f in_rq); rq='${rq}'; AMD='${AMD}'; in_rq set 3 requires 2 >/dev/null \
    && grep -q '^links: \[requires:002\]' '${rq}'/tasks/backlog/003-top.md \
    && ! grep -q 'required-by' '${rq}'/tasks/backlog/001-base.md \
    && in_rq set 3 requires '' >/dev/null && ! grep -q 'requires:' '${rq}'/tasks/backlog/003-top.md"
( cd "${rq}" && RITE_NO_BRANCH=1 "${AMD}" set 3 requires 1 >/dev/null && "${AMD}" group sprint s1 >/dev/null \
  && "${AMD}" set 3 points 3 >/dev/null && "${AMD}" s1 3 >/dev/null )
assert "rite group start is refused while a sprint ticket requires one outside it, naming it" \
  bash -c "$(declare -f in_rq); rq='${rq}'; AMD='${AMD}'; out=\$(in_rq group start s1 2>&1); [ \$? -ne 0 ] \
    && printf '%s' \"\$out\" | grep -q 's1 can.t start: 003 requires 001 (backlog)' \
    && grep -q '^started: \"pending\"' '${rq}'/tasks/backlog/s1/_group.md"
assert "with the required ticket in the sprint, it starts" \
  bash -c "$(declare -f in_rq); rq='${rq}'; AMD='${AMD}'; in_rq set 1 points 2 >/dev/null && in_rq s1 1 >/dev/null \
    && in_rq group start s1 | grep -q 's1 started'"
( cd "${rq}" && RITE_NO_BRANCH=1 "${AMD}" new "Loner" >/dev/null && "${AMD}" new "Friend" >/dev/null \
  && "${AMD}" link 3 5 >/dev/null )
assert "rite tree of a ticket in a started sprint says the tree leaves it out" \
  bash -c "$(declare -f in_rq); rq='${rq}'; AMD='${AMD}'; in_rq tree 3 2>&1 | grep -q 'started sprint s1, which the tree leaves out'"
assert "rite tree shows the started sprint's ticket as a stub where something links to it" \
  bash -c "$(declare -f in_rq); rq='${rq}'; AMD='${AMD}'; in_rq tree 5 | grep -q '^└ ─ ─ 003 Top · in started sprint s1\$'"
assert "rite tree of a ticket with no links says so" \
  bash -c "$(declare -f in_rq); rq='${rq}'; AMD='${AMD}'; in_rq tree 4 | grep -qx '004 requires nothing and has no related tickets'"
rm -rf "${rq}"
tr="$(mktemp -d)"
( cd "${tr}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init trees --local-only >/dev/null \
  && for t in Alpha Beta Gamma Delta; do "${AMD}" new "$t" >/dev/null; done \
  && "${AMD}" link 1 2 --type requires >/dev/null && "${AMD}" link 2 3 --type requires >/dev/null \
  && "${AMD}" link 1 4 >/dev/null \
  && for c in todo doing "done"; do RITE_NO_BRANCH=1 "${AMD}" "$c" 3 >/dev/null; done )
assert "rite tree --done draws requires solid and related dashed, with where each is and what waits" \
  bash -c "cd '${tr}' && [ \"\$('${AMD}' tree 4 --done)\" = \"\$(printf '%s\\n' \\
    '001 Alpha · backlog  waits on 002' \\
    '├──── 002 Beta · backlog' \\
    '│     └──── 003 Gamma · done ✓' \\
    '└ ─ ─ 004 Delta · backlog  ◀')\" ]"
assert "rite tree leaves done tickets out without --done" \
  bash -c "cd '${tr}' && [ \"\$('${AMD}' tree 4)\" = \"\$(printf '%s\\n' \\
    '001 Alpha · backlog  waits on 002' \\
    '├──── 002 Beta · backlog' \\
    '└ ─ ─ 004 Delta · backlog  ◀')\" ]"
assert "rite tree of a ticket linked only to done ones says they're done" \
  bash -c "cd '${tr}' && '${AMD}' new Epsilon >/dev/null && '${AMD}' link 5 3 >/dev/null \
    && '${AMD}' tree 5 | grep -qx 'Everything 005 is linked to is done (rite tree 005 --done shows it)'"
# Ticket 261: parent and child, from the top ancestor down.
( cd "${tr}" && "${AMD}" new Zeta --parent 1 >/dev/null && "${AMD}" new Eta --parent 6 >/dev/null )
assert "rite tree --children draws the hierarchy from the top, the ticket marked" \
  bash -c "cd '${tr}' && [ \"\$('${AMD}' tree 7 --children)\" = \"\$(printf '%s\\n' \\
    '001 Alpha · backlog' \\
    '└──── 006 Zeta · backlog' \\
    '      └──── 007 Eta · backlog  ◀')\" ]"
assert "...every hierarchy on the board without a ticket, and none for a ticket with no parent or children" \
  bash -c "cd '${tr}' && '${AMD}' tree --children | head -1 | grep -qx '001 Alpha · backlog' \
    && '${AMD}' tree 2 --children | grep -qx '002 has no parent and no children'"
rm -rf "${tr}"

echo "auto priority (ticket 215):"
ap="$(mktemp -d)"
( cd "${ap}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init autop --local-only >/dev/null \
  && "${AMD}" auto-priority > "${ap}.default" && "${AMD}" auto-priority off >/dev/null \
  && "${AMD}" due-to-todo 0 >/dev/null \
  && for t in Goal Middle Base Extra; do "${AMD}" new "$t" >/dev/null; done \
  && "${AMD}" set 1 priority high >/dev/null \
  && "${AMD}" link 1 2 --type requires >/dev/null && "${AMD}" link 2 3 --type requires >/dev/null )
in_ap() { (cd "${ap}" && RITE_NO_BRANCH=1 "${AMD}" "$@"); }
assert "auto priority is on by default (ticket 252), and a board can switch it off" \
  bash -c "$(declare -f in_ap); ap='${ap}'; AMD='${AMD}'; grep -qx 'auto priority is on' '${ap}.default' \
    && grep -q 'due dates raise a ticket a step each quarter from its creation to its due date' '${ap}.default' \
    && in_ap auto-priority | grep -qx 'auto priority is off' \
    && grep -q '^priority: \"low\"' '${ap}'/tasks/backlog/003-base.md"
assert "rite auto-priority on raises a chain in the setting's own commit, saying why" \
  bash -c "$(declare -f in_ap); ap='${ap}'; AMD='${AMD}'; out=\$(in_ap auto-priority on) \
    && printf '%s' \"\$out\" | grep -q '003 raised to high: 002 requires it' \
    && grep -q '^priority: \"high\"' '${ap}'/tasks/backlog/003-base.md \
    && grep -q '^priority-from: \"002\"' '${ap}'/tasks/backlog/003-base.md \
    && git -C '${ap}' show --stat HEAD | grep -q '003-base.md'"
assert "a new requires link raises in the link's own commit" \
  bash -c "$(declare -f in_ap); ap='${ap}'; AMD='${AMD}'; in_ap link 3 4 --type requires >/dev/null \
    && grep -q '^priority: \"high\"' '${ap}'/tasks/backlog/004-extra.md \
    && git -C '${ap}' log --format=%s -1 | grep -q '^link: 003 -> 004'"
assert "rite show says why a priority was raised" \
  bash -c "$(declare -f in_ap); ap='${ap}'; AMD='${AMD}'; in_ap show 4 2>&1 >/dev/null | grep -q 'raised by auto priority, 003 requires it (own: low)'"
assert "with override on, a priority set by hand pins the ticket" \
  bash -c "$(declare -f in_ap); ap='${ap}'; AMD='${AMD}'; in_ap set 3 priority low | grep -q pinned \
    && grep -q '^priority: \"low\"' '${ap}'/tasks/backlog/003-base.md \
    && grep -q '^priority: \"low\"' '${ap}'/tasks/backlog/004-extra.md"
assert "rite set <ref> priority auto hands it back to auto priority" \
  bash -c "$(declare -f in_ap); ap='${ap}'; AMD='${AMD}'; in_ap set 3 priority auto >/dev/null \
    && grep -q '^priority: \"high\"' '${ap}'/tasks/backlog/003-base.md"
assert "with override off, a priority set by hand is only a floor" \
  bash -c "$(declare -f in_ap); ap='${ap}'; AMD='${AMD}'; in_ap priority-override off >/dev/null \
    && in_ap set 3 priority low >/dev/null && grep -q '^priority: \"high\"' '${ap}'/tasks/backlog/003-base.md"
assert "rite auto-priority off puts raised tickets back" \
  bash -c "$(declare -f in_ap); ap='${ap}'; AMD='${AMD}'; in_ap auto-priority off | grep -q '002 back to low' \
    && grep -q '^priority: \"low\"' '${ap}'/tasks/backlog/002-middle.md \
    && grep -q '^priority: \"high\"' '${ap}'/tasks/backlog/001-goal.md \
    && ! grep -q 'priority-from' '${ap}'/tasks/backlog/00[234]-*.md"
ap_week="$(date -u -d '+7 days' +%F)"
ap_day="$(date -u +%F)"
assert "a ticket due the day it's made is top at once, in its own commit, saying why (ticket 252)" \
  bash -c "$(declare -f in_ap); ap='${ap}'; AMD='${AMD}'; in_ap auto-priority on >/dev/null \
    && in_ap new Deadline --due '${ap_day}' >/dev/null 2>&1 \
    && grep -q '^priority: \"top\"' '${ap}'/tasks/backlog/005-deadline.md \
    && grep -q '^priority-from: \"due\"' '${ap}'/tasks/backlog/005-deadline.md \
    && in_ap show 5 2>&1 >/dev/null | grep -q 'raised by auto priority, due ${ap_day} (own: low)'"
assert "a due date counts quarters from creation: a week away from a ticket made today is no raise yet" \
  bash -c "$(declare -f in_ap); ap='${ap}'; AMD='${AMD}'; in_ap set 5 due '${ap_week}' >/dev/null 2>&1 \
    && grep -q '^priority: \"low\"' '${ap}'/tasks/backlog/005-deadline.md \
    && sed -i 's/^created: .*/created: \"2026-01-01\"/' '${ap}'/tasks/backlog/005-deadline.md \
    && in_ap set 5 points 1 >/dev/null 2>&1 \
    && grep -q '^priority: \"top\"' '${ap}'/tasks/backlog/005-deadline.md"
ap_today="$(date -u +%F)"
assert "entering todo and doing records when; back in the backlog clears it (ticket 252)" \
  bash -c "$(declare -f in_ap); ap='${ap}'; AMD='${AMD}'; in_ap todo 4 >/dev/null 2>&1 \
    && grep -q '^todo-since: \"${ap_today}\"' '${ap}'/tasks/todo/004-extra.md \
    && in_ap doing 4 >/dev/null 2>&1 \
    && grep -q '^todo-since: \"${ap_today}\"' '${ap}'/tasks/doing/004-extra.md \
    && grep -q '^doing-since: \"${ap_today}\"' '${ap}'/tasks/doing/004-extra.md \
    && in_ap todo 4 >/dev/null 2>&1 && ! grep -q '^doing-since' '${ap}'/tasks/todo/004-extra.md \
    && in_ap backlog 4 >/dev/null 2>&1 && ! grep -q 'since' '${ap}'/tasks/backlog/004-extra.md"
assert "rite auto-priority limits sets the time limits, and a ticket over them is raised" \
  bash -c "$(declare -f in_ap); ap='${ap}'; AMD='${AMD}'; in_ap set 4 priority auto >/dev/null 2>&1; \
    in_ap todo 4 >/dev/null 2>&1 \
    && sed -i 's/^todo-since: .*/todo-since: \"2026-01-01\"/' '${ap}'/tasks/todo/004-extra.md \
    && in_ap auto-priority limits 8 4 | grep -q 'time limits: 8 days in todo, 4 days in doing' \
    && grep -q '^priority: \"top\"' '${ap}'/tasks/todo/004-extra.md \
    && grep -q '^priority-from: \"todo\"' '${ap}'/tasks/todo/004-extra.md \
    && in_ap auto-priority | grep -q 'time limits: 8 days in todo'"
rm -rf "${ap}" "${ap}.default"
# A priority that moves on its own (a due date reached), then a change:
# the settings carry-over run before every change used to commit the move
# under its own name, `settings: migrate "from documents"` (ticket 331).
pa="$(mktemp -d)"
( cd "${pa}" && git init -q && git config user.email t@t.co && git config user.name t \
  && git config commit.gpgsign false && RITE_YES=1 "${AMD}" init prio --local-only >/dev/null \
  && RITE_NO_BRANCH=1 "${AMD}" new Later --due "$(date -u -d '+90 days' +%F)" >/dev/null 2>&1 \
  && sed -i "s/^due: .*/due: \"$(date -u +%F)\"/" tasks/backlog/001-later.md \
  && git commit -q -a -m "due today, by hand (ticket 331)" )
assert "a priority that moved by itself gets its own commit, priority: auto, before the change's (ticket 331)" \
  bash -c "cd '${pa}' && RITE_NO_BRANCH=1 '${AMD}' new Other >/dev/null 2>&1 \
    && [ \"\$(git log --format=%s -2)\" = \"\$(printf 'new: 002 \"Other\"\npriority: auto')\" ] \
    && grep -q '^priority: \"top\"' tasks/backlog/001-later.md \
    && ! git log --format=%s | grep -q '^settings: migrate'"
rm -rf "${pa}"

echo "due dates on the board (ticket 253):"
dd="$(mktemp -d)"
( cd "${dd}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init dated --local-only >/dev/null \
  && "${AMD}" group epic checkout >/dev/null && "${AMD}" group sprint s1 >/dev/null )
in_dd() { (cd "${dd}" && RITE_NO_BRANCH=1 "${AMD}" "$@"); }
dd_soon="$(date -u -d '+1 day' +%F)"
dd_later="$(date -u -d '+20 days' +%F)"
assert "rite due-to-todo says 3 days by default" \
  bash -c "$(declare -f in_dd); dd='${dd}'; AMD='${AMD}'; in_dd due-to-todo | grep -qx 'a ticket due within 3 days goes straight to todo'"
assert "a ticket made due soon goes straight to todo, recording when; one due later stays" \
  bash -c "$(declare -f in_dd); dd='${dd}'; AMD='${AMD}'; \
    in_dd new Soon --due '${dd_soon}' 2>&1 | grep -q 'moved straight to todo' \
    && grep -q '^todo-since: ' '${dd}'/tasks/todo/001-soon.md \
    && in_dd new Later --due '${dd_later}' >/dev/null 2>&1 && ls '${dd}'/tasks/backlog/002-later.md >/dev/null"
assert "giving a backlog ticket a due date soon moves it to todo; a draft stays" \
  bash -c "$(declare -f in_dd); dd='${dd}'; AMD='${AMD}'; \
    in_dd set 2 due '${dd_soon}' | grep -q 'moved straight to todo' && ls '${dd}'/tasks/todo/002-later.md >/dev/null \
    && in_dd new Idea --draft --due '${dd_soon}' >/dev/null 2>&1 && ls '${dd}'/tasks/backlog/003-idea.md >/dev/null"
assert "a ticket with a due date can't go into an epic, nor get one while in it; a sprint can hold it, with a warning" \
  bash -c "$(declare -f in_dd); dd='${dd}'; AMD='${AMD}'; \
    in_dd new Dated --due '${dd_later}' >/dev/null 2>&1 \
    && in_dd set 4 epic checkout 2>&1 | grep -q \"can't be in the epic checkout\" \
    && in_dd new Filed >/dev/null 2>&1 && in_dd set 5 epic checkout >/dev/null \
    && in_dd set 5 due '${dd_later}' 2>&1 | grep -q \"can't be in the epic checkout\" \
    && in_dd set 4 points 3 >/dev/null && in_dd set 4 epic s1 >/dev/null \
    && in_dd s1 2>&1 >/dev/null | grep -q 'warning: s1 holds tickets with due dates: 004 (due ${dd_later})'"
assert "rite due-to-todo 0 keeps every ticket in the backlog" \
  bash -c "$(declare -f in_dd); dd='${dd}'; AMD='${AMD}'; in_dd due-to-todo 0 >/dev/null \
    && grep -qx 'due-to-todo-days = 0' '${dd}'/.settings \
    && in_dd new Now --due '$(date -u +%F)' >/dev/null 2>&1 && ls '${dd}'/tasks/backlog/006-now.md >/dev/null"
assert "rite backlog-always-shown on and off" \
  bash -c "$(declare -f in_dd); dd='${dd}'; AMD='${AMD}'; in_dd backlog-always-shown on | grep -q \"can't be folded away\" \
    && grep -qx 'backlog-always-shown = on' '${dd}'/.settings \
    && in_dd backlog-always-shown off >/dev/null && ! grep -q 'backlog-always-shown' '${dd}'/.settings"
rm -rf "${dd}"

echo "your own columns (ticket 224):"
cl="$(mktemp -d)"
( cd "${cl}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init cols --local-only >/dev/null \
  && for t in One Two Three; do "${AMD}" new "$t" >/dev/null; done )
in_cl() { (cd "${cl}" && RITE_NO_BRANCH=1 "${AMD}" "$@"); }
assert "rite column add puts a column where asked, in one commit, and rite board shows it" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; n=\$(git -C '${cl}' rev-list --count HEAD) \
    && in_cl column add Review --kind doing --after doing 2>/dev/null | grep -qx 'added Review (review/, doing)' \
    && [ \$(git -C '${cl}' rev-list --count HEAD) -eq \$((n + 1)) ] \
    && [ -d '${cl}'/tasks/review ] \
    && [ \"\$(in_cl board --plain | grep -E '^[A-Z]' | tr '\\n' ' ')\" = 'BACKLOG TODO DOING REVIEW DONE ' ]"
assert "rite column lists name, folder, kind, WIP limit and count" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; in_cl column | grep -qx 'Review  review/  doing, no WIP limit  (0)'"
assert "a ticket moves into the column by its name, one step at a time, and stays locked" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; in_cl todo 1 >/dev/null && in_cl doing 1 >/dev/null \
    && ! in_cl done 1 2>/dev/null >/dev/null \
    && in_cl review 1 | grep -q 'moved 001-one.md -> review/' \
    && grep -q '^locked-by: \"t@t.co\"' '${cl}'/tasks/review/001-one.md"
assert "a doing column's own WIP limit refuses one more, and rite move --force gets past it" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; in_cl column wip review 1 >/dev/null \
    && in_cl todo 2 >/dev/null && in_cl doing 2 >/dev/null \
    && in_cl review 2 2>&1 | grep -q 'review/ already has 1 of 1' \
    && in_cl move 2 review --force | grep -q 'moved 002-two.md -> review/'"
assert "a rename keeps the folder and the board shows the new name" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; in_cl column rename todo Ready | grep -qx 'todo/ is now called Ready' \
    && [ -d '${cl}'/tasks/todo ] && in_cl board --plain | grep -qx 'READY' \
    && in_cl draft 3 | grep -q 'draft'"
assert "only an added column can go, and its tickets need somewhere to go first" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; in_cl column remove done 2>&1 | grep -q 'standard columns' \
    && in_cl column remove review 2>&1 | grep -q 'move-to' \
    && in_cl column remove review --move-to doing | grep -q 'moved 2 ticket(s) to doing/' \
    && [ ! -d '${cl}'/tasks/review ] && ls '${cl}'/tasks/doing | grep -q 001-one.md \
    && ! grep -q 'wip-limit-review' '${cl}'/.settings"
assert "rite column move reorders, and a column can't be named like an epic" \
  bash -c "$(declare -f in_cl); cl='${cl}'; AMD='${AMD}'; in_cl column add Triage --kind backlog --after done >/dev/null 2>&1 \
    && in_cl column move triage --before backlog | grep -qx 'Triage | Backlog | Ready | Doing | Done' \
    && in_cl group epic checkout >/dev/null && ! in_cl column add Checkout --kind doing 2>/dev/null"
rm -rf "${cl}"

echo "flow rules, landing column and reset (ticket 225):"
fl="$(mktemp -d)"
( cd "${fl}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init flows --local-only >/dev/null \
  && "${AMD}" column add Triage --kind backlog --before backlog >/dev/null 2>&1 \
  && "${AMD}" column add Review --kind doing --after doing >/dev/null 2>&1 \
  && "${AMD}" new One >/dev/null && "${AMD}" new Two >/dev/null )
in_fl() { (cd "${fl}" && RITE_NO_BRANCH=1 "${AMD}" "$@"); }
assert "rite column landing triage puts new tickets in Triage" \
  bash -c "$(declare -f in_fl); fl='${fl}'; AMD='${AMD}'; in_fl column landing triage | grep -qx 'new tickets land in Triage' \
    && in_fl new Three | grep -q '^created triage/003-three.md' && [ -f '${fl}'/tasks/triage/003-three.md ]"
assert "from Triage a ticket can't skip to todo until the move is allowed, and then can" \
  bash -c "$(declare -f in_fl); fl='${fl}'; AMD='${AMD}'; ! in_fl todo 3 >/dev/null 2>&1 \
    && in_fl column allow triage todo | grep -qx 'allowed Triage → To do' \
    && in_fl todo 3 | grep -q 'moved 003-three.md -> todo/' \
    && in_fl column | grep -qx 'flow: next, plus Triage → To do'"
assert "an extra move is one way, and disallow takes it back" \
  bash -c "$(declare -f in_fl); fl='${fl}'; AMD='${AMD}'; ! in_fl triage 3 >/dev/null 2>&1 \
    && in_fl column disallow triage todo >/dev/null && ! grep -q '^moves' '${fl}'/.settings"
assert "flow any lets a ticket go from the backlog straight to doing, locked" \
  bash -c "$(declare -f in_fl); fl='${fl}'; AMD='${AMD}'; in_fl column flow any | grep -qx 'flow: any' \
    && in_fl doing 1 | grep -q 'moved 001-one.md -> doing/' \
    && grep -q '^locked-by: \"t@t.co\"' '${fl}'/tasks/doing/001-one.md"
assert "rite column reset moves tickets by kind and puts every setting back, in one commit" \
  bash -c "$(declare -f in_fl); fl='${fl}'; AMD='${AMD}'; in_fl review 1 >/dev/null && in_fl new Four >/dev/null \
    && n=\$(git -C '${fl}' rev-list --count HEAD) \
    && out=\$(in_fl column reset) \
    && printf '%s' \"\$out\" | grep -q 'moved 001-one.md -> doing/' \
    && printf '%s' \"\$out\" | grep -q 'moved 004-four.md -> backlog/' \
    && [ \$(git -C '${fl}' rev-list --count HEAD) -eq \$((n + 1)) ] \
    && [ ! -d '${fl}'/tasks/review ] && [ ! -d '${fl}'/tasks/triage ] \
    && ! grep -qE '^(columns|flow|moves|new-tickets)' '${fl}'/.settings \
    && [ \"\$(in_fl board --plain | grep -E '^[A-Z]' | tr '\\n' ' ')\" = 'BACKLOG TODO DOING DONE ' ]"
rm -rf "${fl}"

echo "duplicate:"
assert "rite duplicate copies a ticket" \
  "${AMD}" duplicate 1
copy="$(grep -l '^clone-of: "001"$' tasks/*/*.md | head -1)"
assert "the copy records what it is a copy of" \
  bash -c "test -n '${copy}' && grep -q '^clone-of: \"001\"$' '${copy}'"
assert "the copy lands in the backlog" \
  bash -c "case '${copy}' in tasks/backlog/*) true;; *) false;; esac"
copy_id="$(basename "${copy}" | cut -c1-3)"
assert "the original lists the copy in clones" \
  bash -c "grep -q '^clones: \[${copy_id}\]$' tasks/*/001-first-task.md"
assert "a copy is not a plain relation" \
  bash -c "! grep -q '${copy_id}' <(grep '^related:' tasks/*/001-first-task.md)"
assert "the copy keeps the title" \
  bash -c "grep -q '^title: \"First task\"$' '${copy}'"
assert "duplicating an unknown ticket is refused" \
  bash -c "! '${AMD}' duplicate 99"

# `rite set` and `rite group` shipped with no coverage here, which is how the
# suite came to disagree with the tool. They get their own repository so the
# epic and sprint directories cannot disturb the id sequencing above.
echo "set:"
work="$(mktemp -d)"
( cd "${work}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init gadgets --local-only >/dev/null && "${AMD}" tags add urgent >/dev/null \
  && "${AMD}" new "First" >/dev/null && "${AMD}" new "Second" >/dev/null )
assert "set points sizes a ticket" \
  bash -c "cd '${work}' && '${AMD}' set 001 points 5 | grep -q 'sized 5' && grep -q '^points: \"5\"\$' tasks/backlog/001-first.md"
assert "set points rejects a non-Fibonacci value" \
  bash -c "cd '${work}' && '${AMD}' set 001 points 4 2>&1 | grep -q 'invalid points'"
assert "set points clears with an empty value, unaffected by the Fibonacci check" \
  bash -c "cd '${work}' && '${AMD}' set 001 points '' | grep -q 'sized' && grep -q '^points: \"\"\$' tasks/backlog/001-first.md"
( cd "${work}" && "${AMD}" set 001 points 5 >/dev/null )
assert "set title rewrites the title, not the filename" \
  bash -c "cd '${work}' && '${AMD}' set 001 title 'Renamed thing' | grep -q 'retitled' && grep -q '^title: \"Renamed thing\"\$' tasks/backlog/001-first.md"
assert "set order ranks a ticket fractionally" \
  bash -c "cd '${work}' && '${AMD}' set 002 order 1.5 | grep -q 'ranked 1.5' && grep -q '^order: \"1.5\"\$' tasks/backlog/002-second.md"
assert "an unknown field is rejected" \
  bash -c "cd '${work}' && ! '${AMD}' set 001 nonsense 1"

echo "epics and sprints:"
assert "an empty backlog says so" \
  bash -c "cd '${work}' && '${AMD}' group list | grep -q 'no epics or sprints'"
assert "group epic creates one" \
  bash -c "cd '${work}' && '${AMD}' group epic checkout --description 'the checkout flow' | grep -q 'created epic checkout' && test -f tasks/backlog/checkout/_group.md"
assert "group sprint records its length" \
  bash -c "cd '${work}' && '${AMD}' group sprint sprint-1 --days 10 | grep -q 'created sprint sprint-1 (10 days)'"
assert "a duplicate group is refused" \
  bash -c "cd '${work}' && ! '${AMD}' group epic checkout"
assert "set epic files the ticket into the epic directory" \
  bash -c "cd '${work}' && '${AMD}' set 002 epic checkout | grep -q 'filed under checkout' && test -f tasks/backlog/checkout/002-second.md && grep -q '^epic: \"checkout\"\$' tasks/backlog/checkout/002-second.md"
# Sizing keys are written on demand, not stubbed at creation — an unsized
# ticket has no `points:` line at all, which is what an epic accepts and a
# sprint refuses.
assert "an epic takes an unsized ticket" \
  bash -c "cd '${work}' && ! grep -q '^points:' tasks/backlog/checkout/002-second.md"
assert "a sprint refuses an unsized ticket" \
  bash -c "cd '${work}' && '${AMD}' set 002 epic sprint-1 2>&1 | grep -q 'needs points before it can go in sprint-1'"
assert "a sprint takes a sized one" \
  bash -c "cd '${work}' && '${AMD}' set 001 epic sprint-1 | grep -q 'filed under sprint-1' && test -f tasks/backlog/sprint-1/001-first.md"
assert "group list totals tickets and points" \
  bash -c "cd '${work}' && '${AMD}' group list | grep -qE 'sprint-1 +sprint +10d +pending +1 ticket\(s\), 5 point\(s\)'"
assert "starting a sprint says so" \
  bash -c "cd '${work}' && '${AMD}' group start sprint-1 | grep -q 'started'"
# A started sprint is a commitment (ticket 083): it takes nothing more and
# gives nothing back — its tickets only move on, to todo.
assert "a started sprint gives nothing back" \
  bash -c "cd '${work}' && '${AMD}' set 001 epic checkout 2>&1 | grep -q 'only move on to todo'"
assert "a started sprint takes nothing more, even sized" \
  bash -c "cd '${work}' && '${AMD}' set 002 points 3 >/dev/null && '${AMD}' set 002 epic sprint-1 2>&1 | grep -q 'no more tickets'"
assert "a started sprint refuses an unsized ticket too" \
  bash -c "cd '${work}' && '${AMD}' new 'Unsized' >/dev/null && ! '${AMD}' set 003 epic sprint-1 >/dev/null 2>&1"
assert "archiving straight out of a started sprint is refused" \
  bash -c "cd '${work}' && '${AMD}' abandon 001 2>&1 | grep -q 'move it on to todo before archiving'"
assert "a ticket outside the sprint is archived" \
  bash -c "cd '${work}' && '${AMD}' abandon 002 | grep -q 'archived'"
assert "rite abandon with no ref lists the archive" \
  bash -c "cd '${work}' && '${AMD}' abandon | grep -q Second"
# The bug this guards: a sprint's points fell as work progressed, because the
# count scanned backlog/ only and a ticket in doing/ has no epic directory.
assert "a sprint keeps its points when work starts" \
  bash -c "cd '${work}' && before=\$('${AMD}' group list | grep sprint-1) && '${AMD}' todo 001 --no-branch >/dev/null && '${AMD}' doing 001 >/dev/null && after=\$('${AMD}' group list | grep sprint-1) && [ \"\${before}\" = \"\${after}\" ]"
assert "rite group rename renames a group and relabels its tickets" \
  bash -c "cd '${work}' && '${AMD}' group rename checkout payments | grep -q 'renamed checkout -> payments' \
    && test -d tasks/backlog/payments && ! test -d tasks/backlog/checkout \
    && ! grep -rq --exclude-dir=archive '^epic: \"checkout\"' tasks && '${AMD}' group list | grep -q payments \
    && '${AMD}' group rename payments checkout >/dev/null"
assert "starting an unknown group fails" \
  bash -c "cd '${work}' && ! '${AMD}' group start nope"
assert "rite <epic> <ref> files a ticket, same as set epic" \
  bash -c "cd '${work}' && '${AMD}' new 'Verb filed' >/dev/null \
    && id=\$(ls tasks/backlog | grep verb-filed | cut -d- -f1) \
    && '${AMD}' checkout \${id} | grep -q 'filed under checkout' \
    && test -f tasks/backlog/checkout/\${id}-verb-filed.md"
assert "rite <epic> with no ref lists that epic's tickets" \
  bash -c "cd '${work}' && '${AMD}' checkout | grep -q 'Verb filed'"
assert "rite <epic> --tag narrows an epic listing to tasks carrying that tag" \
  bash -c "cd '${work}' && '${AMD}' new 'Verb filed and urgent' -t urgent --no-edit >/dev/null \
    && id=\$(ls tasks/backlog | grep verb-filed-and-urgent | cut -d- -f1) \
    && '${AMD}' checkout \${id} >/dev/null \
    && '${AMD}' checkout --tag urgent | grep -q 'Verb filed and urgent' \
    && ! '${AMD}' checkout --tag urgent | grep -q '\[.*\] Verb filed\$'"
assert "an epic cannot be created with a column's name" \
  bash -c "cd '${work}' && '${AMD}' group epic todo 2>&1 | grep -q collides"
assert "an epic cannot be created named abandon" \
  bash -c "cd '${work}' && '${AMD}' group epic abandon 2>&1 | grep -q collides"
assert "rite group empty moves every ticket in a group back to loose backlog, no precondition" \
  bash -c "cd '${work}' && '${AMD}' --plain checkout | grep -q '\[' \
    && '${AMD}' group empty checkout | grep -q 'emptied into backlog' \
    && '${AMD}' --plain checkout | grep -q '(empty)'"
assert "a nonempty group cannot be deleted" \
  bash -c "cd '${work}' && '${AMD}' new 'Back in checkout' --no-edit >/dev/null \
    && id=\$(ls tasks/backlog | grep back-in-checkout | cut -d- -f1) \
    && '${AMD}' checkout \${id} >/dev/null \
    && '${AMD}' group delete checkout 2>&1 | grep -q '1 ticket' \
    && test -d tasks/backlog/checkout"
assert "an emptied group can be deleted" \
  bash -c "cd '${work}' && '${AMD}' group empty checkout >/dev/null \
    && '${AMD}' group delete checkout | grep -q 'deleted checkout' \
    && ! test -d tasks/backlog/checkout"
assert "a group whose tickets are all done can be deleted, and they keep its name" \
  bash -c "cd '${work}' && '${AMD}' group epic finished >/dev/null \
    && '${AMD}' new 'Finished work' --no-edit >/dev/null \
    && id=\$(ls tasks/backlog | grep finished-work | cut -d- -f1) \
    && '${AMD}' finished \${id} >/dev/null \
    && '${AMD}' todo \${id} >/dev/null && '${AMD}' doing \${id} --force >/dev/null \
    && '${AMD}' group delete finished 2>&1 | grep -q '1 ticket(s) not done' \
    && '${AMD}' done \${id} >/dev/null \
    && '${AMD}' group delete finished | grep -q 'deleted finished' \
    && ! test -d tasks/backlog/finished \
    && grep -q 'epic: \"finished\"' tasks/done/\${id}-*.md"
assert "deleting an unknown group is an error" \
  bash -c "cd '${work}' && ! '${AMD}' group delete nope"
rm -rf "${work}"

echo "statistics (ticket 233):"
stats_repo="$(mktemp -d)"
( cd "${stats_repo}" && git init -q -b main && git config user.email t@t.co && git config user.name t \
  && git config commit.gpgsign false && RITE_YES=1 "${AMD}" init statstest --local-only >/dev/null \
  && git add -A && git commit -q -m "board for ticket 233" )
assert "rite stats counts tickets raised, done and closed from git history" \
  bash -c "cd '${stats_repo}' && '${AMD}' new 'Counted' --no-edit >/dev/null && '${AMD}' new 'Dropped' --no-edit >/dev/null \
    && '${AMD}' todo 1 --no-branch >/dev/null && '${AMD}' doing 1 >/dev/null && '${AMD}' done 1 >/dev/null \
    && '${AMD}' abandon 2 >/dev/null \
    && '${AMD}' stats --days 7 | grep -qx 'last 7 days: 2 raised · 1 done · 1 closed' \
    && '${AMD}' stats --days 7 | grep -q 'done   1\$'"
rm -rf "${stats_repo}"

echo "recurring tickets and due dates (ticket 143):"
recur_repo="$(mktemp -d)"
( cd "${recur_repo}" && git init -q -b main && git config user.email t@t.co && git config user.name t \
  && git config commit.gpgsign false && RITE_YES=1 "${AMD}" init recurtest --local-only >/dev/null \
  && "${AMD}" due-to-todo 0 >/dev/null \
  && git add -A && git commit -q -m "board for ticket 143" )
assert "a recurring ticket done makes the next one, due after today, in the same commit" \
  bash -c "cd '${recur_repo}' && '${AMD}' new 'Renew the certificate' --recur Monthly --due 2026-01-01 --no-edit >/dev/null \
    && grep -q '^recur: \"monthly\"' tasks/backlog/001-renew-the-certificate.md \
    && '${AMD}' show 1 2>&1 >/dev/null | grep -q 'overdue: it was due 2026-01-01' \
    && '${AMD}' --plain board | grep -q 'overdue:2026-01-01 repeats:monthly' \
    && '${AMD}' todo 1 --no-branch >/dev/null && '${AMD}' doing 1 >/dev/null \
    && out=\$('${AMD}' done 1) && printf '%s\\n' \"\$out\" | grep -q '001 repeats: made 002, due 20' \
    && f=tasks/backlog/002-renew-the-certificate.md && grep -q '^recurs-from: \"001\"' \$f \
    && grep -q '^recur: \"monthly\"' \$f \
    && due=\$(sed -n 's/^due: \"\\(.*\\)\"/\\1/p' \$f) && [ \"\$due\" \\> \"\$(date -u +%F)\" ] \
    && git show --stat --pretty=format: HEAD | grep -q '002-renew-the-certificate.md'"
assert "rite set due and recur check what they're given; empty clears" \
  bash -c "cd '${recur_repo}' && ! '${AMD}' set 2 due tomorrow 2>/dev/null \
    && ! '${AMD}' set 2 recur sometimes 2>/dev/null \
    && '${AMD}' set 2 recur 'every 2 weeks' | grep -q '002 repeats fortnightly' \
    && '${AMD}' set 2 recur '' | grep -q \"002 doesn't repeat\" \
    && ! grep -q '^recur:' tasks/backlog/002-renew-the-certificate.md"
rm -rf "${recur_repo}"

echo "every ticket linked to a ticket (ticket 121):"
links_repo="$(mktemp -d)"
( cd "${links_repo}" && git init -q -b main && git config user.email t@t.co && git config user.name t \
  && git config commit.gpgsign false && RITE_YES=1 "${AMD}" init linkstest --local-only >/dev/null \
  && for t in One Two Three; do "${AMD}" new "$t" --no-edit >/dev/null; done \
  && "${AMD}" link 1 2 --type requires >/dev/null && "${AMD}" link 3 1 --type supersedes --one-way >/dev/null )
assert "rite links lists every link either way, saying which are one-sided" \
  bash -c "cd '${links_repo}' && out=\$('${AMD}' links 1) \
    && printf '%s\\n' \"\$out\" | grep -q '^  requires  *002 Two · backlog\$' \
    && printf '%s\\n' \"\$out\" | grep -q '^  is superseded by  *003 Three · backlog  (recorded on 003 only)\$' \
    && '${AMD}' links 2 | grep -q 'is required by  *001 One' \
    && '${AMD}' related 3 | grep -q 'supersedes  *001 One'"
assert "...and --json gives the same, for scripts" \
  bash -c "cd '${links_repo}' && '${AMD}' links 1 --json | grep -q '\"kind\": \"superseded-by\"' \
    && '${AMD}' links 1 --json | grep -q '\"recorded_here\": false'"
rm -rf "${links_repo}"

echo "commit-and-push:"
commit_repo="$(mktemp -d)"
commit_origin="$(mktemp -d)/origin.git"
mkdir -p "${commit_origin}" && git init -q --bare "${commit_origin}"
( cd "${commit_repo}" && git init -q -b main && git config user.email t@t.co && git config user.name t \
  && git remote add origin "${commit_origin}" \
  && RITE_YES=1 "${AMD}" init committest --local-only >/dev/null )
assert "rite new commits both the new ticket and the counter bump" \
  bash -c "cd '${commit_repo}' && '${AMD}' new 'First ticket' --no-edit >/dev/null \
    && git show --stat --pretty=format: HEAD | grep -q '001-first-ticket.md' \
    && git show --stat --pretty=format: HEAD | grep -q '.next-id'"
assert "that commit was pushed to origin" \
  bash -c "cd '${commit_repo}' && [ \"\$(git rev-parse HEAD)\" = \"\$(git rev-parse origin/main)\" ]"
assert "a file staged by hand beforehand is never swept into the next commit" \
  bash -c "cd '${commit_repo}' && echo dummy > dummy.md && git add dummy.md \
    && '${AMD}' todo 1 --no-branch >/dev/null \
    && ! git show --stat --pretty=format: HEAD | grep -q dummy.md \
    && git status --porcelain=v1 | grep -q '^A  dummy.md\$'"
assert "RITE_COMMIT=0 suppresses the commit entirely" \
  bash -c "cd '${commit_repo}' && RITE_COMMIT=0 '${AMD}' doing 1 >/dev/null \
    && git status --porcelain=v1 | grep -q '001-first-ticket.md'"
assert "the move itself still happened despite RITE_COMMIT=0" \
  bash -c "cd '${commit_repo}' && test -f tasks/doing/001-first-ticket.md"
assert "rite group empty commits every ticket it moves in one commit" \
  bash -c "cd '${commit_repo}' && '${AMD}' group epic squad >/dev/null \
    && '${AMD}' squad 1 >/dev/null && '${AMD}' new 'Second ticket' --no-edit >/dev/null \
    && '${AMD}' squad 2 >/dev/null \
    && before=\$(git rev-list --count HEAD) \
    && '${AMD}' group empty squad >/dev/null \
    && after=\$(git rev-list --count HEAD) \
    && [ \$((after - before)) -eq 1 ] \
    && git show --stat --pretty=format: HEAD | grep -q '001-first-ticket.md' \
    && git show --stat --pretty=format: HEAD | grep -q '002-second-ticket.md'"
assert "rite group empty --to todo moves a group's backlog tickets to todo in one commit; drafts stay (ticket 256)" \
  bash -c "cd '${commit_repo}' && '${AMD}' group epic wave >/dev/null \
    && for t in Third Fourth; do '${AMD}' new \"\$t\" --no-edit >/dev/null; done \
    && '${AMD}' new Fifth --draft --no-edit >/dev/null 2>&1 \
    && for id in 3 4 5; do '${AMD}' wave \$id >/dev/null; done \
    && before=\$(git rev-list --count HEAD) \
    && out=\$('${AMD}' group empty wave --to todo 2>/tmp/amd-wave.err) \
    && after=\$(git rev-list --count HEAD) && [ \$((after - before)) -eq 1 ] \
    && printf '%s' \"\$out\" | grep -q 'wave: 2 ticket(s) moved to todo/ (003, 004)' \
    && grep -q '005 not moved: .*draft' /tmp/amd-wave.err \
    && grep -q '^epic: \"wave\"' tasks/todo/003-third.md && grep -q '^todo-since: ' tasks/todo/004-fourth.md \
    && ls tasks/backlog/wave/005-fifth.md >/dev/null"
assert "rite link commits both ends of the relation in one commit (ticket 054)" \
  bash -c "cd '${commit_repo}' && before=\$(git rev-list --count HEAD) \
    && '${AMD}' link 1 2 >/dev/null \
    && after=\$(git rev-list --count HEAD) \
    && [ \$((after - before)) -eq 1 ] \
    && [ \"\$(git show --name-only --pretty=format: HEAD | grep -c .)\" -eq 2 ] \
    && git show --name-only --pretty=format: HEAD | grep -q '001-first-ticket.md' \
    && git show --name-only --pretty=format: HEAD | grep -q '002-second-ticket.md' \
    && git log -1 --pretty=%s | grep -q '^link: 001 -> 002'"
assert "rite log renders a transition with a real sha" \
  bash -c "cd '${commit_repo}' && '${AMD}' log | grep -qE '^[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}  add .*[0-9a-f]{7,}\$'"
rm -rf "${commit_repo}"

# Ticket 164: a board from before ticket 150, with its counter in
# .settings. The first new ticket moves it to .next-id, and that change
# has to be in the same commit, not left uncommitted.
old_board="$(mktemp -d)"
( cd "${old_board}" && git init -q -b main && git config user.email t@t.co && git config user.name t \
  && git config commit.gpgsign false \
  && RITE_COMMIT=0 RITE_YES=1 "${AMD}" init oldstyle --local-only >/dev/null 2>&1 \
  && rm .next-id && printf 'next-id = 5\n' >> .settings \
  && git add -A && git commit -q -m 'ticket 164 setup: a board from before ticket 150' )
assert "on a board with its counter in .settings, rite new commits the move too, leaving nothing behind" \
  bash -c "cd '${old_board}' && '${AMD}' new 'After the move' --no-edit >/dev/null 2>&1 \
    && test -f tasks/backlog/005-after-the-move.md \
    && [ -z \"\$(git status --porcelain)\" ] \
    && git show --stat --pretty=format: HEAD | grep -q '.settings' \
    && git show --stat --pretty=format: HEAD | grep -q '.next-id' \
    && ! grep -q '^next-id' .settings && [ \"\$(cat .next-id)\" = 6 ]"
assert "...and a dry run that works out numbers changes nothing" \
  bash -c "cd '${old_board}' && git checkout -q HEAD~1 -- .settings && rm .next-id \
    && git add -A && git commit -q -m 'ticket 164 setup: old style again' \
    && src=\"\$(mktemp -d)\" && mkdir -p \"\${src}\"/backlog \"\${src}\"/todo \"\${src}\"/doing \"\${src}\"/done \
    && printf -- '---\nid: \"001\"\ntitle: \"Elsewhere\"\n---\n' > \"\${src}/backlog/001-elsewhere.md\" \
    && cd tasks && '${AMD}' import tickets \"\${src}\" --dry-run 2>&1 | grep -q 'would import 1 ticket' \
    && [ -z \"\$(git status --porcelain)\" ] && rm -rf \"\${src}\""
rm -rf "${old_board}"

# Ticket 142: a board with no remote commits, and doesn't try to push.
local_repo="$(mktemp -d)"
( cd "${local_repo}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init localonly --local-only >/dev/null 2>&1 )
assert "with no remote, a change is committed and nothing is pushed or complained about" \
  bash -c "cd '${local_repo}' && out=\$('${AMD}' new 'Local only' --no-edit 2>&1) \
    && ! printf '%s' \"\$out\" | grep -qiE 'fatal|could not push' \
    && git log -1 --pretty=%s | grep -q 'Local only'"
rm -rf "${local_repo}"

echo "my work (ticket 020):"
mw="$(mktemp -d)"
mw_bin="${mw}/bin"
# Two projects in one folder and a third in another; Sam in all three. The
# folders aren't under the temp directory's own name for scratch boards, so
# put them under a home-like path inside it and point HOME there.
mwh="${mw}/home"
(
  set -e
  export XDG_CONFIG_HOME="${mw}/config" RITE_COMMIT=0 RITE_NO_BRANCH=1
  for spec in "work/alpha" "work/beta" "other/gamma"; do
    dir="${mwh}/${spec%/*}"; name="${spec#*/}"
    mkdir -p "${dir}" && cd "${dir}"
    git init -q -b main "${name}-tickets" && cd "${name}-tickets"
    git config user.email sam@example.com && git config user.name "Sam Lee" && git config commit.gpgsign false
    "${AMD}" init "${name}" --local-only >/dev/null 2>&1
    "${AMD}" register >/dev/null 2>&1 || true
    "${AMD}" new "${name} doing" -a "Sam Lee" --no-edit >/dev/null 2>&1
    "${AMD}" set 1 priority high >/dev/null 2>&1
    "${AMD}" todo 1 >/dev/null 2>&1 && "${AMD}" doing 1 >/dev/null 2>&1
    "${AMD}" new "${name} todo" -a "Sam Lee" --no-edit >/dev/null 2>&1
    "${AMD}" todo 2 >/dev/null 2>&1
    "${AMD}" new "${name} backlog" -a "Sam Lee" --no-edit >/dev/null 2>&1
    "${AMD}" new "${name} someone else" -a "Priya Shah" --no-edit >/dev/null 2>&1
    "${AMD}" todo 4 >/dev/null 2>&1
  done
)
in_mw() { (cd "${mwh}/work/alpha-tickets" && env XDG_CONFIG_HOME="${mw}/config" TMPDIR="${mw_bin}" "${AMD}" "$@"); }
mkdir -p "${mw_bin}"
assert "rite mine lists todo and doing across every project, high priority first" \
  bash -c "$(declare -f in_mw); mw='${mw}'; mwh='${mwh}'; mw_bin='${mw_bin}'; AMD='${AMD}'; out=\$(in_mw mine) \
    && [ \"\$(printf '%s\\n' \"\$out\" | grep -c .)\" -eq 6 ] \
    && printf '%s\\n' \"\$out\" | head -3 | grep -c 'doing  high' | grep -qx 3 \
    && printf '%s\\n' \"\$out\" | grep -q 'gamma' && ! printf '%s\\n' \"\$out\" | grep -q 'backlog\\|someone else'"
assert "rite mine --hide leaves a project out, --all brings it back, --unhide restores it" \
  bash -c "$(declare -f in_mw); mw='${mw}'; mwh='${mwh}'; mw_bin='${mw_bin}'; AMD='${AMD}'; in_mw mine --hide beta >/dev/null \
    && ! in_mw mine 2>/dev/null | grep -q beta && in_mw mine 2>&1 >/dev/null | grep -q '1 hidden project' \
    && in_mw mine --all | grep -q beta && in_mw mine --unhide beta >/dev/null && in_mw mine | grep -q beta"
assert "rite mine --json gives the same list for scripts" \
  bash -c "$(declare -f in_mw); mw='${mw}'; mwh='${mwh}'; mw_bin='${mw_bin}'; AMD='${AMD}'; in_mw mine --json | grep -c '\"project\"' | grep -qx 6"
assert "a TRIAGE ticket heads everyone's My work, top, and its hidden project stays hidden (tickets 248, 252, 263)" \
  bash -c "$(declare -f in_mw); mw='${mw}'; mwh='${mwh}'; mw_bin='${mw_bin}'; AMD='${AMD}'; in_mw mine --hide gamma >/dev/null \
    && (cd '${mwh}/other/gamma-tickets' && env XDG_CONFIG_HOME='${mw}/config' RITE_COMMIT=0 '${AMD}' new 'Look at this issue' --tag TRIAGE --no-edit >/dev/null 2>&1) \
    && out=\$(in_mw mine 2>&1) \
    && printf '%s\\n' \"\$out\" | head -1 | grep -q 'backlog  top .*gamma .*TRIAGE  Look at this issue' \
    && ! printf '%s\\n' \"\$out\" | grep -q 'gamma doing' \
    && grep '^ui.hidden-projects' '${mw}/config/kanrite/config' | grep -q gamma-tickets \
    && in_mw mine --unhide gamma >/dev/null \
    && (cd '${mwh}/other/gamma-tickets' && env XDG_CONFIG_HOME='${mw}/config' RITE_COMMIT=0 '${AMD}' abandon 5 >/dev/null 2>&1; rm -rf tasks/archive)"

echo "archiving a project (ticket 157):"
in_beta() { (cd "${mwh}/work/beta-tickets" && env XDG_CONFIG_HOME="${mw}/config" TMPDIR="${mw_bin}" RITE_NO_BRANCH=1 "${AMD}" "$@"); }
mw_fns="$(declare -f in_mw in_beta); mw='${mw}'; mwh='${mwh}'; mw_bin='${mw_bin}'; AMD='${AMD}'"
assert "rite project archive commits archived = on and rite project marks it" \
  bash -c "${mw_fns}; in_mw project archive beta | grep -q '^archived beta\$' \
    && grep -qx 'archived = on' '${mwh}/work/beta-tickets/.settings' \
    && git -C '${mwh}/work/beta-tickets' log -1 --format=%s | grep -q 'settings: archived' \
    && in_mw project | grep -q 'beta .*(archived)'"
assert "...and it's off rite on this machine, its folder kept (ticket 263)" \
  bash -c "${mw_fns}; ! grep -q beta-tickets '${mw}/config/kanrite/repos' && test -d '${mwh}/work/beta-tickets'"
assert "an archived project refuses changes, naming rite init" \
  bash -c "${mw_fns}; for change in 'new Refused --no-edit' 'todo 3' 'set 3 priority low' 'tags add NEW' 'wip-limit 3'; do \
      in_beta \${change} 2>&1 | grep -q 'beta is archived.*rite init beta' || exit 1; done \
    && [ \"\$(ls '${mwh}/work/beta-tickets/tasks/backlog/' | grep -c refused)\" = 0 ]"
assert "...but still reads" \
  bash -c "${mw_fns}; in_beta board | grep -q 'beta todo' && in_beta show 1 | grep -q 'beta doing' && in_beta tags >/dev/null"
assert "...and is off My work" \
  bash -c "${mw_fns}; out=\$(in_mw mine) && ! printf '%s\\n' \"\$out\" | grep -q beta && printf '%s\\n' \"\$out\" | grep -q alpha"
assert "rite project unarchive brings it back, on rite here again" \
  bash -c "${mw_fns}; in_mw project unarchive beta | grep -q '^unarchived beta\$' \
    && grep -q beta-tickets '${mw}/config/kanrite/repos' \
    && ! grep -q archived '${mwh}/work/beta-tickets/.settings' \
    && in_beta new 'Back again' --no-edit >/dev/null 2>&1 && ls '${mwh}/work/beta-tickets/tasks/backlog/' | grep -q back-again"
# --github: a fake gh logs what it's asked; origin looks like GitHub but
# pushes go to a local bare repository.
mkdir -p "${mw}/ghbin" && git init -q --bare -b main "${mw}/beta-origin.git"
cat > "${mw}/ghbin/gh" <<'GH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_LOG:?}"
GH
chmod +x "${mw}/ghbin/gh"
(
  cd "${mwh}/work/beta-tickets"
  git remote add origin git@github.com:acme/beta-tickets.git
  git config url."${mw}/beta-origin.git".pushInsteadOf git@github.com:acme/beta-tickets.git
  git push -q -u origin main 2>/dev/null
)
gh_beta() { (cd "${mwh}/work/beta-tickets" && env XDG_CONFIG_HOME="${mw}/config" TMPDIR="${mw_bin}" RITE_SYNC=0 GH_LOG="${mw}/gh.log" PATH="${mw}/ghbin:${PATH}" "${AMD}" "$@"); }
mw_fns="${mw_fns}; $(declare -f gh_beta)"
assert "archiving pushes the setting, then archives the tickets and documents repositories on GitHub" \
  bash -c "${mw_fns}; out=\$(gh_beta project archive 2>&1) \
    && printf '%s\\n' \"\$out\" | grep -q 'archived acme/beta-tickets on GitHub' \
    && grep -qx 'repo archive acme/beta-tickets --yes' '${mw}/gh.log' \
    && grep -qx 'repo archive acme/beta-documents --yes' '${mw}/gh.log' \
    && git -C '${mw}/beta-origin.git' show main:.settings | grep -qx 'archived = on'"
assert "unarchiving unarchives on GitHub, then pushes the setting (--github still accepted)" \
  bash -c "${mw_fns}; gh_beta project unarchive --github >/dev/null 2>&1 \
    && grep -qx 'repo unarchive acme/beta-tickets --yes' '${mw}/gh.log' \
    && ! git -C '${mw}/beta-origin.git' show main:.settings | grep -q archived"
rm -rf "${mw}"

echo "work to pick when there's nothing to do (ticket 160):"
# Sam has nothing in todo or doing. alpha: an unassigned todo ticket; in
# the backlog a low unassigned ticket, a medium for Priya, a high draft and
# a low one of Sam's. beta (hidden): a medium unassigned one and a high one
# locked by Priya.
wp="$(mktemp -d)"
wph="${wp}/home"
(
  set -e
  export XDG_CONFIG_HOME="${wp}/config" RITE_COMMIT=0 RITE_NO_BRANCH=1
  mkdir -p "${wph}/work" && cd "${wph}/work"
  for name in alpha beta; do
    git init -q -b main "${name}-tickets"
    ( cd "${name}-tickets" && git config user.email sam@example.com && git config user.name "Sam Lee" \
      && "${AMD}" init "${name}" --local-only >/dev/null 2>&1 && "${AMD}" register >/dev/null 2>&1 || true )
  done
  cd alpha-tickets
  "${AMD}" new "Alpha low" --no-edit >/dev/null 2>&1
  "${AMD}" new "Alpha medium for Priya" -a "Priya Shah" --no-edit >/dev/null 2>&1 && "${AMD}" set 2 priority medium >/dev/null 2>&1
  "${AMD}" new "Alpha high draft" --draft --no-edit >/dev/null 2>&1 && "${AMD}" set 3 priority high >/dev/null 2>&1
  "${AMD}" new "Alpha todo nobody" --no-edit >/dev/null 2>&1 && "${AMD}" todo 4 >/dev/null 2>&1
  "${AMD}" new "Alpha mine" -a "Sam Lee" --no-edit >/dev/null 2>&1
  cd ../beta-tickets
  "${AMD}" new "Beta medium" --no-edit >/dev/null 2>&1 && "${AMD}" set 1 priority medium >/dev/null 2>&1
  "${AMD}" new "Beta high locked" --no-edit >/dev/null 2>&1 && "${AMD}" set 2 priority high >/dev/null 2>&1
  f="$(ls tasks/backlog/002-*.md)"
  sed -i -e 's/^locked-by: .*/locked-by: "priya@example.com"/' -e 's/^locked-until: .*/locked-until: "2099-01-01T00:00:00Z"/' "${f}"
)
in_wp() { (cd "${wph}/work/alpha-tickets" && env XDG_CONFIG_HOME="${wp}/config" TMPDIR="${wp}/bin" "${AMD}" "$@"); }
mkdir -p "${wp}/bin"
in_wp mine --hide beta >/dev/null
assert "(beta starts hidden)" bash -c "grep '^ui.hidden-projects' '${wp}/config/kanrite/config' | grep -q beta-tickets"
assert "with nothing to do, unassigned todo and doing work comes first" \
  bash -c "$(declare -f in_wp); wp='${wp}'; wph='${wph}'; AMD='${AMD}'; out=\$(in_wp mine 2>&1) \
    && printf '%s\\n' \"\$out\" | grep -q 'todo .*Alpha todo nobody' && ! printf '%s\\n' \"\$out\" | grep -q 'backlog'"
(cd "${wph}/work/alpha-tickets" && env XDG_CONFIG_HOME="${wp}/config" RITE_COMMIT=0 "${AMD}" assign 4 "Priya Shah" >/dev/null 2>&1)
assert "failing that, the backlog's highest priority it can take, from a hidden project too" \
  bash -c "$(declare -f in_wp); wp='${wp}'; wph='${wph}'; AMD='${AMD}'; out=\$(in_wp mine 2>&1) \
    && printf '%s\\n' \"\$out\" | grep -q 'work to pick' \
    && printf '%s\\n' \"\$out\" | grep -q 'backlog  medium .*beta .*Beta medium' \
    && ! printf '%s\\n' \"\$out\" | grep -q 'Alpha low\\|Priya\\|draft\\|locked'"
assert "...and the hidden project it found work in stays hidden (ticket 263)" \
  bash -c "grep '^ui.hidden-projects' '${wp}/config/kanrite/config' | grep -q beta-tickets"
assert "a high one it can take wins over every medium" \
  bash -c "$(declare -f in_wp); wp='${wp}'; wph='${wph}'; AMD='${AMD}'; (cd '${wph}/work/alpha-tickets' && env XDG_CONFIG_HOME='${wp}/config' RITE_COMMIT=0 '${AMD}' set 1 priority high >/dev/null) \
    && out=\$(in_wp mine) && printf '%s\\n' \"\$out\" | grep -q 'Alpha low' && ! printf '%s\\n' \"\$out\" | grep -q 'Beta medium'"
assert "--no-unassigned offers only backlog tickets assigned to you, and is remembered" \
  bash -c "$(declare -f in_wp); wp='${wp}'; wph='${wph}'; AMD='${AMD}'; out=\$(in_wp mine --no-unassigned) \
    && printf '%s\\n' \"\$out\" | grep -q 'Alpha mine' && ! printf '%s\\n' \"\$out\" | grep -q 'Alpha low' \
    && grep -qx 'ui.include-unassigned = off' '${wp}/config/kanrite/config' \
    && in_wp mine | grep -q 'Alpha mine' && in_wp mine --unassigned | grep -q 'Alpha low'"
assert "once something is yours, it's back to My work alone" \
  bash -c "$(declare -f in_wp); wp='${wp}'; wph='${wph}'; AMD='${AMD}'; (cd '${wph}/work/alpha-tickets' && env XDG_CONFIG_HOME='${wp}/config' RITE_COMMIT=0 RITE_NO_BRANCH=1 '${AMD}' assign 1 @me >/dev/null && env XDG_CONFIG_HOME='${wp}/config' RITE_COMMIT=0 RITE_NO_BRANCH=1 '${AMD}' todo 1 >/dev/null 2>&1) \
    && out=\$(in_wp mine) && ! printf '%s\\n' \"\$out\" | grep -q 'work to pick' && printf '%s\\n' \"\$out\" | grep -q 'todo .*Alpha low'"
rm -rf "${wp}"

echo "renumber (ticket 146):"
rn="$(mktemp -d)"
( cd "${rn}" && git init -q -b main && git config user.email t@t.co && git config user.name t \
  && git config commit.gpgsign false \
  && RITE_YES=1 "${AMD}" init renum --local-only >/dev/null 2>&1 \
  && "${AMD}" new "First" --no-edit >/dev/null 2>&1 && "${AMD}" new "Second" --no-edit >/dev/null 2>&1 \
  && sed -e 's/^title: .*/title: "Clashing"/' tasks/backlog/002-second.md > tasks/backlog/002-clashing.md \
  && git add tasks/backlog/002-clashing.md && git commit -q -m "a second 002, made outside rite" )
assert "rite renumber shows the plan and changes nothing" \
  bash -c "cd '${rn}' && out=\$('${AMD}' renumber) && printf '%s\\n' \"\$out\" | grep -q '^002: 002-second keeps it' \
    && printf '%s\\n' \"\$out\" | grep -q '002-clashing becomes 003' && test -f tasks/backlog/002-clashing.md"
assert "rite renumber --apply gives the later one a new number, in one commit" \
  bash -c "cd '${rn}' && '${AMD}' renumber --apply >/dev/null 2>&1 \
    && test -f tasks/backlog/003-clashing.md && ! test -f tasks/backlog/002-clashing.md \
    && grep -qx 'id: \"003\"' tasks/backlog/003-clashing.md \
    && git log -1 --pretty=%s | grep -q '^renumber: 002' && [ -z \"\$(git status --porcelain --untracked-files=no)\" ] \
    && '${AMD}' renumber | grep -q 'no two tickets share a number'"
assert "the next new ticket doesn't reuse the moved number" \
  bash -c "cd '${rn}' && '${AMD}' new 'Third' --no-edit 2>&1 | grep -q '004-third'"
rm -rf "${rn}"

echo "recover (ticket 144):"
rv="$(mktemp -d)"
( cd "${rv}" && git init -q -b main && git config user.email sam@example.com && git config user.name Sam \
  && git config commit.gpgsign false \
  && RITE_YES=1 "${AMD}" init recover --local-only >/dev/null 2>&1 \
  && for t in One Two Three; do "${AMD}" new "${t}" --no-edit >/dev/null 2>&1; done \
  && cp tasks/backlog/001-one.md "${rv}.one" && cp tasks/backlog/002-two.md "${rv}.two" \
  && "${AMD}" abandon 1 >/dev/null 2>&1 && "${AMD}" abandon 2 >/dev/null 2>&1 )
assert "rite recover lists abandoned tickets with who and when" \
  bash -c "cd '${rv}' && out=\$('${AMD}' recover) && printf '%s\\n' \"\$out\" | grep -qE '^001  [0-9-]+ [0-9:]+  abandoned by Sam  One\$' \
    && printf '%s\\n' \"\$out\" | grep -q '^002 ' && ! printf '%s\\n' \"\$out\" | grep -q '^003 '"
assert "a ticket still on the board can't be recovered" \
  bash -c "cd '${rv}' && ! '${AMD}' recover 3 >/dev/null 2>&1"
assert "rite recover <id> brings one back into the backlog, exactly as it was" \
  bash -c "cd '${rv}' && '${AMD}' recover 1 | grep -q 'recovered 001-one' \
    && cmp -s tasks/backlog/001-one.md '${rv}.one' && git log -1 --pretty=%s | grep -q '^recover: 001'"
assert "rite recover --all --by restores the rest in one commit" \
  bash -c "cd '${rv}' && before=\$(git rev-list --count HEAD) && '${AMD}' recover --all --by sam >/dev/null \
    && [ \$((\$(git rev-list --count HEAD) - before)) -eq 1 ] \
    && cmp -s tasks/backlog/002-two.md '${rv}.two' && '${AMD}' recover | grep -q 'nothing to recover'"
assert "--since needs a date" \
  bash -c "cd '${rv}' && '${AMD}' recover --since yesterday 2>&1 | grep -q 'YYYY-MM-DD'"
rm -rf "${rv}" "${rv}.one" "${rv}.two"

echo "WIP limits (doing only, ticket 083):"
wip_repo="$(mktemp -d)"
( cd "${wip_repo}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init wiptest --local-only >/dev/null \
  && for t in First Second Third Fourth Fifth; do "${AMD}" new "${t}" --no-edit >/dev/null; done \
  && for n in 1 2 3; do "${AMD}" todo "${n}" --no-branch >/dev/null; done \
  && "${AMD}" doing 1 >/dev/null && "${AMD}" doing 2 >/dev/null )
assert "todo/ is never limited, whatever the settings say" \
  bash -c "cd '${wip_repo}' && RITE_WIP_TODO=1 '${AMD}' todo 4 --no-branch >/dev/null && test -f tasks/todo/004-fourth.md"
assert "doing/ defaults to a WIP limit of 2 with nothing configured at all" \
  bash -c "cd '${wip_repo}' && '${AMD}' doing 3 2>&1 | grep -q 'doing/ already has 2 of 2 allowed'"
assert "the refused move never happened" \
  bash -c "cd '${wip_repo}' && test -f tasks/todo/003-third.md"
assert "rite doing --force overrides the WIP limit" \
  bash -c "cd '${wip_repo}' && '${AMD}' doing 3 --force >/dev/null && test -f tasks/doing/003-third.md"
assert "moving something out of an over-limit column is never blocked" \
  bash -c "cd '${wip_repo}' && '${AMD}' done 3 >/dev/null && test -f tasks/done/003-third.md"
assert "rite wip-limit sets an explicit limit, persisted in .settings" \
  bash -c "cd '${wip_repo}' && '${AMD}' wip-limit 3 >/dev/null && grep -q '^wip-limit = 3\$' .settings"
assert "the new limit takes effect immediately" \
  bash -c "cd '${wip_repo}' && '${AMD}' doing 4 >/dev/null && test -f tasks/doing/004-fourth.md"
assert "rite wip-limit 0 turns doing/'s limit off" \
  bash -c "cd '${wip_repo}' && '${AMD}' wip-limit 0 >/dev/null \
    && '${AMD}' todo 5 --no-branch >/dev/null && '${AMD}' doing 5 >/dev/null \
    && test -f tasks/doing/005-fifth.md"
assert "rite wip-limit rejects a non-numeric value" \
  bash -c "cd '${wip_repo}' && ! '${AMD}' wip-limit nope >/dev/null 2>&1"
assert "a garbled wip-limit value in .settings resets to the default of 2, not unlimited" \
  bash -c "cd '${wip_repo}' && sed -i 's/^wip-limit = 0\$/wip-limit = nope/' .settings \
    && '${AMD}' new \"Sixth\" --no-edit >/dev/null && '${AMD}' todo 6 --no-branch >/dev/null \
    && '${AMD}' doing 6 2>&1 | grep -q 'doing/ already has 4 of 2 allowed'"
rm -rf "${wip_repo}"

echo "import and tag filtering:"
import_src="$(mktemp -d)"
mkdir -p "${import_src}/backlog" "${import_src}/todo" "${import_src}/doing" "${import_src}/done"
cat > "${import_src}/backlog/010-old-backlog-ticket.md" <<'TICKET'
---
id: "010"
title: "Old backlog ticket"
parent: ""
related: []
tags: []
---

body
TICKET
cat > "${import_src}/todo/003-in-progress-work.md" <<'TICKET'
---
id: "003"
title: "In progress work"
parent: ""
related: []
tags: []
---

body
TICKET
cat > "${import_src}/done/007-shipped-feature.md" <<'TICKET'
---
id: "007"
title: "Shipped feature"
parent: ""
related: []
tags: []
---

body
TICKET

import_dst="$(mktemp -d)"
( cd "${import_dst}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init imported --local-only >/dev/null )
assert "rite import tickets is refused outside the board's tasks directory (ticket 173)" \
  bash -c "cd '${import_dst}' && '${AMD}' import '${import_src}' 2>&1 | grep -q 'from the board.s tasks directory: cd .*/tasks'"
assert "...and when the path has nothing to import, saying what it looked for (ticket 251)" \
  bash -c "empty=\$(mktemp -d) && cd '${import_dst}/tasks' \
    && '${AMD}' import tickets \"\$empty\" 2>&1 | grep -q 'has nothing to import: no backlog/, todo/, doing/ or done/ folder, and no tickets' \
    ; rc=\$?; rm -rf \"\$empty\"; exit \$rc"
assert "rite import lands tickets in their original columns with fresh ids, in one commit" \
  bash -c "cd '${import_dst}/tasks' && '${AMD}' import '${import_src}' >/dev/null \
    && test -f backlog/001-old-backlog-ticket.md \
    && test -f todo/002-in-progress-work.md \
    && test -f done/003-shipped-feature.md \
    && git log -1 --format=%s | grep -q '^import: tickets'"
assert "the source tree is untouched by the import" \
  test -f "${import_src}/backlog/010-old-backlog-ticket.md"
assert "a --dry-run import writes nothing" \
  bash -c "cd '${import_dst}/tasks' && '${AMD}' import tickets '${import_src}' --dry-run | grep -q 'nothing to import' \
    && ! test -f backlog/004-old-backlog-ticket.md"
assert "importing the same source again leaves out what's already here, logged to import.log, never committed" \
  bash -c "cd '${import_dst}/tasks' && out=\$('${AMD}' import tickets '${import_src}' 2>&1) \
    && grep -q 'already on this board' <<<\"\$out\" && ! ls backlog/004-* 2>/dev/null \
    && grep -q 'rite import tickets' import.log && grep -q 'Shipped feature.* is already on this board' import.log \
    && ! git status --porcelain --untracked-files=all | grep -q import.log"
# Ticket 251: a source with no backlog/ and tickets loose outside any column,
# as agile-md-hosted's was.
loose_src="$(mktemp -d)"
mkdir -p "${loose_src}/todo" "${loose_src}/doing" "${loose_src}/done"
for t in "124 Loose one" "127 Loose two"; do
  printf -- '---\nid: "%s"\ntitle: "%s"\nparent: ""\nrelated: []\ntags: []\n---\n\nbody\n' "${t%% *}" "${t#* }" \
    > "${loose_src}/${t%% *}-$(printf '%s' "${t#* }" | tr 'A-Z ' 'a-z-').md"
done
printf -- '---\nid: "130"\ntitle: "Started elsewhere"\nparent: ""\nrelated: []\ntags: []\n---\n\nbody\n' \
  > "${loose_src}/todo/130-started-elsewhere.md"
loose_dst="$(mktemp -d)"
( cd "${loose_dst}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init loosetest --local-only >/dev/null )
assert "rite import takes a source missing backlog/, with tickets loose outside any column, saying so (ticket 251)" \
  bash -c "cd '${loose_dst}/tasks' && out=\$('${AMD}' import tickets '${loose_src}' --dry-run 2>&1) \
    && printf '%s\\n' \"\$out\" | grep -q 'has no backlog/ folder, taken as empty' \
    && printf '%s\\n' \"\$out\" | grep -q '2 tickets are loose in .*outside any column: would go into the backlog' \
    && printf '%s\\n' \"\$out\" | grep -q 'would import 3 tickets' \
    && '${AMD}' import tickets '${loose_src}' >/dev/null 2>&1 \
    && test -f todo/001-started-elsewhere.md && test -f backlog/002-loose-one.md && test -f backlog/003-loose-two.md"
rm -rf "${loose_src}" "${loose_dst}"
( cd "${import_dst}" && "${AMD}" tags add duplicate >/dev/null && "${AMD}" new "Old backlog ticket" -t duplicate --no-edit >/dev/null 2>&1 )
assert "rite tag finds nothing for a tag nobody has" \
  bash -c "cd '${import_dst}' && ! '${AMD}' tag NO-SUCH-TAG | grep -q 'Old backlog ticket'"
assert "rite backlog --tag narrows the listed column to tasks carrying that tag" \
  bash -c "cd '${import_dst}' && test \"\$('${AMD}' backlog --tag DUPLICATE --plain | grep -c 'Old backlog ticket')\" = 1 \
    && test \"\$('${AMD}' backlog --plain | grep -c 'Old backlog ticket')\" = 2"
assert "rite board --tag narrows every column to tasks carrying that tag" \
  bash -c "cd '${import_dst}' && out=\$('${AMD}' board --tag DUPLICATE --plain) \
    && echo \"\$out\" | grep -q 'Old backlog ticket' \
    && ! echo \"\$out\" | grep -q 'In progress work' \
    && test \"\$(echo \"\$out\" | grep -c 'Old backlog ticket')\" = 1"
assert "rite <destination> --tag with no matches lists an empty column, not an error" \
  bash -c "cd '${import_dst}' && '${AMD}' backlog --tag NO-SUCH-TAG --plain | grep -q '(empty)'"

echo "ideas (ticket 131):"
# A project with its documents repository and a bare origin. Sam is git's identity in the clones; Priya votes through git's
# environment config.
ix="$(mktemp -d)"
ix_env="XDG_CONFIG_HOME=${ix}/config RITE_NO_INPUT=1"
(
  set -e
  export XDG_CONFIG_HOME="${ix}/config"
  cd "${ix}"
  git init -q --bare -b main origin-documents.git
  git init -q -b main idp-tickets
  git clone -q origin-documents.git idp-documents 2>/dev/null
  for r in idp-tickets idp-documents; do
    git -C "${r}" config user.email sam@acme.io && git -C "${r}" config user.name "Sam Lee" && git -C "${r}" config commit.gpgsign false
  done
  ( cd idp-tickets && "${AMD}" init idp --local-only >/dev/null 2>&1 && "${AMD}" register . >/dev/null 2>&1 )
  mkdir -p idp-documents/ideas && echo "# Old idea" > idp-documents/ideas/old-idea.md
  git -C idp-documents add -A && git -C idp-documents commit -q -m "seed for ticket 131" && git -C idp-documents push -q origin main 2>/dev/null
)
in_ix() { (cd "${ix}/idp-tickets" && env ${ix_env} "${AMD}" "$@"); }
ix_fns="$(declare -f in_ix); ix='${ix}'; ix_env='${ix_env}'; AMD='${AMD}'"
assert "rite idea new puts an idea forward with your name, committed and pushed" \
  bash -c "${ix_fns}; in_ix idea new 'Dark mode' | grep -q 'put forward ideas/dark-mode.md in idp-documents' \
    && grep -q '^author: \"Sam Lee <sam@acme.io>\"' '${ix}/idp-documents/ideas/dark-mode.md' \
    && git -C '${ix}/origin-documents.git' log -1 --format=%s main | grep -q '^idea: Dark mode'"
assert "...--anonymous leaves the name off" \
  bash -c "${ix_fns}; in_ix idea new 'Quiet tweak' --anonymous >/dev/null && ! grep -q author '${ix}/idp-documents/ideas/quiet-tweak.md'"
assert "votes are clicks: ++ then - leaves +1, and +++ stops at +3" \
  bash -c "${ix_fns}; in_ix idea vote dark ++ | grep -q 'your vote on Dark mode is +2' \
    && in_ix idea vote dark - | grep -q 'is +1; its score is +1' \
    && in_ix idea vote dark +++ | grep -q 'is +3' \
    && ! in_ix idea vote dark + 2>/dev/null"
assert "another person's vote counts beside yours, and a negative one sinks" \
  bash -c "${ix_fns}; in_ix idea vote quiet + >/dev/null \
    && (cd '${ix}/idp-tickets' && env ${ix_env} GIT_CONFIG_COUNT=2 GIT_CONFIG_KEY_0=user.email GIT_CONFIG_VALUE_0=priya@acme.io \
         GIT_CONFIG_KEY_1=user.name GIT_CONFIG_VALUE_1='Priya Shah' '${AMD}' idea vote quiet --- >/dev/null) \
    && grep -q 'Sam Lee <sam@acme.io> +1\",\"Priya Shah <priya@acme.io> -3' '${ix}/idp-documents/ideas/quiet-tweak.md' \
    && in_ix idea show quiet | grep -q 'votes: Sam Lee +1, Priya Shah -3'"
assert "rite idea lists best first, the old idea in the middle and the negative one last" \
  bash -c "${ix_fns}; out=\$(in_ix idea) \
    && printf '%s\\n' \"\$out\" | head -1 | grep -q '+3 .*Dark mode — Sam Lee' \
    && printf '%s\\n' \"\$out\" | tail -1 | grep -q -- '-2 .*Quiet tweak'"
assert "rite idea note adds a dated note; show prints votes and notes" \
  bash -c "${ix_fns}; in_ix idea note dark 'Follow the OS setting' | grep -q 'added a note to Dark mode' \
    && out=\$(in_ix idea show dark) && printf '%s\\n' \"\$out\" | grep -q 'votes: Sam Lee +3' \
    && printf '%s\\n' \"\$out\" | grep -q 'Sam Lee (20[0-9-]*): Follow the OS setting'"
assert "rite idea ticket makes a backlog ticket from it, linked both ways" \
  bash -c "${ix_fns}; out=\$(in_ix idea ticket dark 2>&1) && printf '%s\\n' \"\$out\" | grep -q 'Dark mode -> ticket 001' \
    && grep -q '^source: \"idea:idp-documents/ideas/dark-mode.md\"' '${ix}'/idp-tickets/tasks/backlog/001-dark-mode.md \
    && grep -q '^tickets: \[\"idp-tickets#001\"\]' '${ix}/idp-documents/ideas/dark-mode.md'"
assert "rite idea ticket --as epic makes an empty epic named after it, its write-up the description (ticket 231)" \
  bash -c "${ix_fns}; printf '\\nFollow the OS.\\nOr a switch.\\n' >> '${ix}/idp-documents/ideas/quiet-tweak.md' \
    && git -C '${ix}/idp-documents' commit -qam 'write-up for ticket 231' && git -C '${ix}/idp-documents' push -q origin main 2>/dev/null \
    && in_ix idea ticket quiet --as epic | grep -q 'Quiet tweak -> epic Quiet tweak' \
    && grep -q '^kind: \"epic\"' '${ix}/idp-tickets/tasks/backlog/Quiet tweak/_group.md' \
    && grep -q 'From the idea .*Quiet tweak.*: Follow the OS. Or a switch.' '${ix}/idp-tickets/tasks/backlog/Quiet tweak/_group.md' \
    && grep -q '^epics: \[\"idp-tickets#Quiet tweak\"\]' '${ix}/idp-documents/ideas/quiet-tweak.md'"
assert "...--as sprint --name --days makes a pending sprint of that length" \
  bash -c "${ix_fns}; in_ix idea ticket dark --as sprint --name s1 --days 10 | grep -q 'Dark mode -> sprint s1' \
    && grep -q '^days: \"10\"' '${ix}/idp-tickets/tasks/backlog/s1/_group.md' \
    && grep -q '^started: \"pending\"' '${ix}/idp-tickets/tasks/backlog/s1/_group.md' \
    && grep -q '^sprints: \[\"idp-tickets#s1\"\]' '${ix}/idp-documents/ideas/dark-mode.md'"
echo "reading documents from the command line (ticket 179):"
(
  cd "${ix}/idp-documents" && mkdir -p docs
  printf '# Idp\n\nStart with [the guide](docs/guide.md).\n' > README.md
  printf '# Guide\n\nSee [setup](setup.md) and [home](../README.md).\nThe Widget lives here.\n' > docs/guide.md
  printf '# Setup\n\nInstall it.\n' > docs/setup.md
  git add -A && git commit -q -m "docs for ticket 179" && git push -q origin main 2>/dev/null
)
assert "rite docs start prints the README until a page is pinned; --path gives its path" \
  bash -c "${ix_fns}; in_ix docs start | head -1 | grep -qx '# Idp' \
    && in_ix docs start --path | grep -q 'idp-documents/README.md\$'"
assert "rite docs tree follows links from the start page, each page once" \
  bash -c "${ix_fns}; out=\$(in_ix docs tree) && [ \"\$out\" = \"\$(printf 'Idp  README.md\\n  Guide  docs/guide.md\\n    Setup  docs/setup.md')\" ]"
assert "rite docs search finds lines across documents, ignoring case" \
  bash -c "${ix_fns}; in_ix docs search widget | grep -qx 'idp-documents/docs/guide.md:4: The Widget lives here.' \
    && in_ix docs search no such words 2>&1 | grep -q 'nothing matches'"
assert "a pinned start page is what rite docs start prints" \
  bash -c "${ix_fns}; echo 'start-page = docs/setup.md' > '${ix}/idp-documents/.agile-md' \
    && in_ix docs start | head -1 | grep -qx '# Setup' && rm '${ix}/idp-documents/.agile-md'"
assert "rite docs pin keeps your own pins in the documents repository, pushed (ticket 232)" \
  bash -c "${ix_fns}; in_ix docs pin idp-documents/docs/guide.md | grep -q 'pin: idp-documents/docs/guide.md' \
    && grep -qx 'idp-documents/docs/guide.md' '${ix}/idp-documents/.agile-md-pins/sam@acme.io' \
    && git -C '${ix}/origin-documents.git' log -1 --format=%s main | grep -q '^pin: idp-documents/docs/guide.md' \
    && in_ix docs pins | grep -qx 'idp-documents/docs/guide.md' \
    && ! in_ix docs pin idp-documents/docs/nothing.md 2>/dev/null"
assert "...and rite docs unpin takes it off" \
  bash -c "${ix_fns}; in_ix docs unpin idp-documents/docs/guide.md | grep -q '^unpin:' \
    && ! test -e '${ix}/idp-documents/.agile-md-pins' && in_ix docs pins | grep -q 'nothing pinned'"
assert "rite group close ends a sprint: the unstarted go back, a report is pushed to documents (ticket 052)" \
  bash -c "${ix_fns}; in_ix group sprint s9 --days 5 >/dev/null \
    && in_ix new 'Close me' --no-edit >/dev/null && in_ix new 'Left behind' --no-edit >/dev/null \
    && a=\$(ls '${ix}'/idp-tickets/tasks/backlog | grep close-me | cut -d- -f1) \
    && b=\$(ls '${ix}'/idp-tickets/tasks/backlog | grep left-behind | cut -d- -f1) \
    && in_ix set \$a points 3 >/dev/null && in_ix set \$b points 2 >/dev/null \
    && in_ix s9 \$a >/dev/null && in_ix s9 \$b >/dev/null && in_ix group start s9 >/dev/null \
    && in_ix todo \$a --no-branch >/dev/null && in_ix doing \$a --force >/dev/null && in_ix done \$a >/dev/null \
    && out=\$(in_ix group close s9 2>&1) \
    && printf '%s\\n' \"\$out\" | grep -q 's9 closed: 3 of 5 points done, 1 ticket(s) not done' \
    && printf '%s\\n' \"\$out\" | grep -q \"back to the backlog: \$b\" \
    && test -f '${ix}'/idp-tickets/tasks/backlog/\$b-left-behind.md \
    && grep -q '^started: \"closed\"' '${ix}/idp-tickets/tasks/backlog/s9/_group.md' \
    && ls '${ix}'/idp-documents/sprint/history/ | grep -q '^20[0-9]*T[0-9]*Z-s9.svg\$' \
    && grep -q '^## Not done' '${ix}'/idp-documents/sprint/history/*-s9.md \
    && git -C '${ix}/origin-documents.git' log -1 --format=%s main | grep -q '^sprint: s9 closed' \
    && ! in_ix s9 \$b 2>/dev/null"
assert "rite group backfill pulls sized, unassigned backlog tickets into a running sprint" \
  bash -c "${ix_fns}; in_ix group sprint s10 --days 5 >/dev/null && in_ix group start s10 >/dev/null \
    && b=\$(ls '${ix}'/idp-tickets/tasks/backlog | grep left-behind | cut -d- -f1) \
    && in_ix group backfill s10 --points 2 | grep -q \"backfilled s10: \$b\" \
    && test -f '${ix}'/idp-tickets/tasks/backlog/s10/\$b-left-behind.md \
    && ! in_ix group backfill s9 --points 2 2>/dev/null"
rm -rf "${ix}"

echo "importing documents (ticket 079):"
# A project folder: proj-tickets, and proj-documents with a bare origin; and
# a tree of old documents somewhere else.
dm="$(mktemp -d)"
dm_env="XDG_CONFIG_HOME=${dm}/config"
(
  set -e
  export XDG_CONFIG_HOME="${dm}/config"
  cd "${dm}"
  git init -q --bare -b main origin-documents.git
  git init -q -b main proj-tickets && git clone -q origin-documents.git proj-documents 2>/dev/null
  for r in proj-tickets proj-documents; do
    git -C "${r}" config user.email t@t.co && git -C "${r}" config user.name t && git -C "${r}" config commit.gpgsign false
  done
  ( cd proj-tickets && RITE_COMMIT=0 "${AMD}" init proj --local-only >/dev/null 2>&1 && "${AMD}" register . >/dev/null 2>&1 )
  mkdir -p proj-documents/adr && echo "# Already here" > proj-documents/adr/0001-x.md
  git -C proj-documents add -A && git -C proj-documents commit -q -m "seed for ticket 079" && git -C proj-documents push -q origin main 2>/dev/null
  mkdir -p old/adr old/img old/.obsidian
  printf '# Old docs\n![d](img/d.png) [a](adr/0001-x.md)\n' > old/README.md
  echo "# X" > old/adr/0001-x.md
  echo "# Y" > old/adr/0002-y.md
  echo png > old/img/d.png
  echo unused > old/img/unused.png
  echo "# hidden" > old/.obsidian/h.md
)
assert "rite import documents is refused outside a documents repository (ticket 173)" \
  bash -c "cd '${dm}/old' && env ${dm_env} '${AMD}' import documents '${dm}/old' 2>&1 | grep -q 'from inside a documents repository'"
assert "rite import documents --dry-run lists the files and writes nothing" \
  bash -c "cd '${dm}/proj-documents' && out=\"\$(env ${dm_env} '${AMD}' import documents '${dm}/old' --dry-run 2>&1)\" \
    && grep -q 'would add README.md' <<<\"\${out}\" && grep -q 'would add img/d.png' <<<\"\${out}\" \
    && grep -q 'skipped adr/0001-x.md: already in proj-documents, not overwritten' <<<\"\${out}\" \
    && ! grep -q 'unused.png\|obsidian' <<<\"\${out}\" \
    && ! test -e README.md && ! test -e import.log"
assert "from a folder of the repository, it imports there, logs what it left out, commits on main and pushes" \
  bash -c "mkdir -p '${dm}/proj-documents/docs/old' && cd '${dm}/proj-documents/docs/old' \
    && echo '# Clash' > README.md && git add README.md && git -c commit.gpgsign=false commit -q -m 'ticket 173 setup: a clash' && git push -q 2>/dev/null \
    && out=\"\$(env ${dm_env} '${AMD}' import documents '${dm}/old' 2>&1)\" \
    && grep -q 'imported 3 files into proj-documents on main, pushed' <<<\"\${out}\" \
    && grep -q 'skipped docs/old/README.md: already in proj-documents' <<<\"\${out}\" \
    && grep -q 'docs/old/README.md: already in proj-documents, not overwritten' import.log \
    && grep -qx '# Clash' README.md \
    && git -C '${dm}/origin-documents.git' show main:docs/old/adr/0002-y.md | grep -q '# Y' \
    && git -C '${dm}/origin-documents.git' show main:docs/old/img/d.png >/dev/null \
    && test -z \"\$(git -C '${dm}/proj-documents' branch --list 'chore/*')\" \
    && test -z \"\$(git -C '${dm}/proj-documents' status --porcelain)\""
assert "...and refuses a documents clone that isn't on its main branch" \
  bash -c "git -C '${dm}/proj-documents' remote set-head origin main \
    && git -C '${dm}/proj-documents' switch -q -c elsewhere && echo '# Z' > '${dm}/old/adr/0003-z.md' \
    && cd '${dm}/proj-documents' && env ${dm_env} '${AMD}' import documents '${dm}/old' 2>&1 | grep -q 'not main; switch to main first' \
    && git -C '${dm}/proj-documents' switch -q main && rm '${dm}/old/adr/0003-z.md'"
assert "...and a documents clone with uncommitted changes" \
  bash -c "echo changed >> '${dm}/proj-documents/adr/0001-x.md' \
    && cd '${dm}/proj-documents' && ! env ${dm_env} '${AMD}' import documents '${dm}/old' 2>&1 | grep -q 'imported' \
    && git -C '${dm}/proj-documents' checkout -q -- adr/0001-x.md"
rm -rf "${dm}"

echo "locking:"
lock_repo="$(mktemp -d)"
( cd "${lock_repo}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init locktest --local-only >/dev/null \
  && "${AMD}" new "Lockable ticket" --no-edit >/dev/null \
  && "${AMD}" lock 1 >/dev/null )
assert "rite lock records the holder in the ticket's own frontmatter" \
  bash -c "cd '${lock_repo}' && '${AMD}' show 1 | grep -q 'locked-by: \"t@t.co\"'"
assert "a different git identity is refused editing a leased ticket" \
  bash -c "cd '${lock_repo}' && git config user.email other@example.com && '${AMD}' set 1 points 3 2>&1 | grep -q 'leased by t@t.co'"
assert "a different git identity is refused moving a leased ticket" \
  bash -c "cd '${lock_repo}' && git config user.email other@example.com && '${AMD}' todo 1 --no-branch 2>&1 | grep -q 'leased by t@t.co'"
assert "the holder's own identity can still edit the leased ticket" \
  bash -c "cd '${lock_repo}' && git config user.email t@t.co && '${AMD}' set 1 points 3 | grep -q 'sized 3'"
assert "a different identity cannot plain-unlock someone else's lease" \
  bash -c "cd '${lock_repo}' && git config user.email other@example.com && '${AMD}' unlock 1 2>&1 | grep -q 'leased by t@t.co'"
assert "a different git identity is refused filing a leased ticket under an epic" \
  bash -c "cd '${lock_repo}' && git config user.email t@t.co && '${AMD}' group epic checkout >/dev/null \
    && git config user.email other@example.com && '${AMD}' checkout 1 2>&1 | grep -q 'leased by t@t.co'"
assert "a different git identity is refused abandoning a leased ticket" \
  bash -c "cd '${lock_repo}' && git config user.email other@example.com && '${AMD}' abandon 1 2>&1 | grep -q 'leased by t@t.co'"
assert "unlock --force breaks someone else's lease" \
  bash -c "cd '${lock_repo}' && git config user.email other@example.com && '${AMD}' unlock 1 --force | grep -q 'lease broken'"
assert "the lease is gone after being broken" \
  bash -c "cd '${lock_repo}' && ! '${AMD}' show 1 | grep -q 'locked-by: \"t@t.co\"'"
# A ticket is locked exactly while it's in doing (ticket 156): todo only
# means it's next. Leaving doing, or reaching done, backlog or the archive,
# gives your own lease back (ticket 092); nobody else's is touched.
assert "rite todo takes no lease (ticket 156)" \
  bash -c "cd '${lock_repo}' && git config user.email t@t.co && '${AMD}' todo 1 --no-branch >/dev/null \
    && '${AMD}' show 1 | grep -q 'locked-by: \"\"'"
assert "rite doing takes a now-free lease on entry" \
  bash -c "cd '${lock_repo}' && '${AMD}' doing 1 >/dev/null 2>&1 && '${AMD}' show 1 | grep -q 'locked-by: \"t@t.co\"'"
assert "doing -> todo gives your lease back (ticket 156)" \
  bash -c "cd '${lock_repo}' && '${AMD}' todo 1 --no-branch >/dev/null 2>&1 \
    && '${AMD}' show 1 | grep -q 'locked-by: \"\"'"
assert "rite done releases your lease" \
  bash -c "cd '${lock_repo}' && '${AMD}' doing 1 >/dev/null 2>&1 && '${AMD}' done 1 >/dev/null 2>&1 \
    && '${AMD}' show 1 | grep -q 'locked-by: \"\"' && '${AMD}' show 1 | grep -q 'locked-until: \"\"'"
assert "...in the same commit as the move" \
  bash -c "cd '${lock_repo}' && git show HEAD --name-status | grep -q 'done/001-lockable-ticket.md' \
    && git show HEAD:tasks/done/001-lockable-ticket.md | grep -q 'locked-by: \"\"'"
( cd "${lock_repo}" && "${AMD}" new "Handed back" --no-edit >/dev/null 2>&1 && "${AMD}" new "Dropped" --no-edit >/dev/null 2>&1 \
  && "${AMD}" todo 2 --no-branch >/dev/null 2>&1 && "${AMD}" todo 3 --no-branch >/dev/null 2>&1 \
  && "${AMD}" lock 2 >/dev/null 2>&1 && "${AMD}" lock 3 >/dev/null 2>&1 )
assert "moving back to the backlog releases your lease" \
  bash -c "cd '${lock_repo}' && '${AMD}' show 2 | grep -q 'locked-by: \"t@t.co\"' \
    && '${AMD}' backlog 2 >/dev/null 2>&1 && '${AMD}' show 2 | grep -q 'locked-by: \"\"'"
assert "abandoning releases your lease" \
  bash -c "cd '${lock_repo}' && '${AMD}' abandon 3 >/dev/null 2>&1 \
    && grep -q 'locked-by: \"\"' tasks/archive/003-dropped.md"
assert "a move to done never clears someone else's (lapsed) lease" \
  bash -c "cd '${lock_repo}' && '${AMD}' todo 2 --no-branch >/dev/null 2>&1 && '${AMD}' doing 2 >/dev/null 2>&1 \
    && sed -i 's/^locked-by: .*/locked-by: \"other@example.com\"/; s/^locked-until: .*/locked-until: \"2000-01-01T00:00:00Z\"/' tasks/doing/002-handed-back.md \
    && '${AMD}' done 2 >/dev/null 2>&1 && '${AMD}' show 2 | grep -q 'locked-by: \"other@example.com\"'"

# Ticket 110: show says what a recorded lease means, on stderr, so the
# ticket on stdout is untouched. 002 now carries someone else's lapsed lease.
assert "show calls someone else's lapsed lease lapsed, not held" \
  bash -c "cd '${lock_repo}' && '${AMD}' show 2 2>&1 >/dev/null \
    | grep -qx 'locked by other@example.com until 2000-01-01T00:00:00Z (lapsed): anyone can change it now'"
assert "...and a lapsed lease never blocks a write" \
  bash -c "cd '${lock_repo}' && '${AMD}' set 2 points 5 | grep -q 'sized 5'"
assert "show says when your own lock has lapsed, and how to take it again" \
  bash -c "cd '${lock_repo}' && sed -i 's/^locked-by: .*/locked-by: \"t@t.co\"/' tasks/done/002-handed-back.md \
    && '${AMD}' show 2 2>&1 >/dev/null | grep -q 'your lock lapsed at 2000-01-01T00:00:00Z — .rite lock 002. to keep working'"
assert "show says a lock in force is yours" \
  bash -c "cd '${lock_repo}' && '${AMD}' new 'Held now' --no-edit >/dev/null 2>&1 && '${AMD}' lock 4 >/dev/null \
    && '${AMD}' show 4 2>&1 >/dev/null | grep -q '^locked by you until '"
assert "show says no lock for a ticket with no lease, and stdout is just the file" \
  bash -c "cd '${lock_repo}' && ! '${AMD}' show 1 2>&1 >/dev/null | grep -qi lock \
    && '${AMD}' show 4 2>/dev/null | cmp -s - tasks/backlog/004-held-now.md"
assert "show says who created a ticket (ticket 155)" \
  bash -c "cd '${lock_repo}' && '${AMD}' show 1 2>&1 >/dev/null | grep -q '^created by '"
assert "a new ticket records created-by from git (ticket 155)" \
  bash -c "cd '${lock_repo}' && grep -q '^created-by: \"' tasks/*/001-*.md"

echo "pipes:"
assert "the board survives a closed pipe" \
  bash -c "'${AMD}' board --plain | head -2 >/dev/null 2>/tmp/amd-pipe.err; ! grep -q 'panicked' /tmp/amd-pipe.err"
assert "showing a task survives a closed pipe" \
  bash -c "'${AMD}' show 1 | head -1 >/dev/null 2>/tmp/amd-pipe.err; ! grep -q 'panicked' /tmp/amd-pipe.err"

echo "completions:"
assert "bash completions are valid bash" \
  bash -c "'${AMD}' completions bash 2>/dev/null > comp.bash && bash -n comp.bash"
assert "zsh completions start with the compdef line" \
  bash -c "'${AMD}' completions zsh 2>/dev/null | head -1 | grep -q '^#compdef rite amd$'"
assert "fish completions are fish syntax" \
  bash -c "'${AMD}' completions fish 2>/dev/null | grep -q '^complete -c rite'"
assert "completions know the subcommands" \
  bash -c "'${AMD}' completions bash 2>/dev/null | grep -q 'link' && '${AMD}' completions fish 2>/dev/null | grep -q 'templates'"
assert "completions offer the change types" \
  bash -c "'${AMD}' completions fish 2>/dev/null | grep -q 'feat'"
assert "completions offer the branch types" \
  bash -c "'${AMD}' completions fish 2>/dev/null | grep -q 'bugfix'"
assert "completions offer the set fields" \
  bash -c "'${AMD}' completions bash 2>/dev/null | grep -q 'points epic order title'"
assert "the shell is taken from \$SHELL when not given" \
  bash -c "SHELL=/bin/zsh '${AMD}' completions 2>/dev/null | head -1 | grep -q '^#compdef rite amd$'"
assert "an unknown shell errors with the choices" \
  bash -c "SHELL=/bin/nope '${AMD}' completions 2>&1 | grep -q 'rite completions bash|zsh|fish'"
assert "the install hint goes to stderr, not into the script" \
  bash -c "'${AMD}' completions bash 2>/dev/null | grep -qv '^# install it with:' && '${AMD}' completions bash 2>&1 >/dev/null | grep -q 'install it with'"
assert "completions work without a board" \
  bash -c "cd '${nongit}' && '${AMD}' completions bash | grep -q '_rite'"
# Epic names and added columns are destination verbs, so completion offers
# them, read from the board at TAB time (ticket 028).
dc="$(mktemp -d)"
( cd "${dc}" && git init -q && git config user.email t@t.co && git config user.name t \
  && RITE_YES=1 "${AMD}" init dests --local-only >/dev/null \
  && RITE_NO_BRANCH=1 "${AMD}" group epic checkout >/dev/null \
  && RITE_NO_BRANCH=1 "${AMD}" group sprint s1 >/dev/null \
  && RITE_NO_BRANCH=1 "${AMD}" group epic draft >/dev/null \
  && RITE_NO_BRANCH=1 "${AMD}" group epic "Launch plan" >/dev/null \
  && RITE_NO_BRANCH=1 "${AMD}" column add Review --kind doing --after doing >/dev/null 2>&1 )
# The bash script's hook, run as bash would on TAB after "rite <word>".
tab() { # word -> the completions offered, one a line
  bash -c "source comp.bash; cd '${dc}'; COMP_WORDS=(rite '$1'); COMP_CWORD=1; _rite_destinations rite '$1' rite; printf '%s\\n' \"\${COMPREPLY[@]}\"" 2>/dev/null
}
"${AMD}" completions bash 2>/dev/null > comp.bash
dc_path="${PATH}"
PATH="$(dirname "${AMD}"):${PATH}"
assert "rite __destinations lists the epics, sprints and added columns (ticket 028)" \
  bash -c "cd '${dc}' && out=\"\$('${AMD}' __destinations)\" && echo \"\${out}\" | grep -qx checkout \
    && echo \"\${out}\" | grep -qx s1 && echo \"\${out}\" | grep -qx review"
assert "it leaves out a name a command has, and one with a space" \
  bash -c "cd '${dc}' && ! '${AMD}' __destinations | grep -qx draft && ! '${AMD}' __destinations | grep -q 'Launch'"
assert "it prints nothing, and succeeds, where there's no board" \
  bash -c "cd '${nongit}' && [ -z \"\$('${AMD}' __destinations 2>&1)\" ]"
assert "bash completes an epic name as the command" \
  bash -c "$(declare -f tab); dc='${dc}'; tab che | grep -qx checkout"
assert "bash still completes the commands alongside them" \
  bash -c "$(declare -f tab); dc='${dc}'; tab do | grep -qx doing && tab do | grep -qx done"
( cd "${dc}" && RITE_NO_BRANCH=1 "${AMD}" group epic payments >/dev/null )
assert "an epic made since the script was sourced is offered on the next TAB" \
  bash -c "$(declare -f tab); dc='${dc}'; tab pay | grep -qx payments"
assert "zsh and fish ask rite for them too" \
  bash -c "'${AMD}' completions zsh 2>/dev/null | grep -q 'rite __destinations' && '${AMD}' completions fish 2>/dev/null | grep -q 'rite __destinations'"
PATH="${dc_path}"
rm -rf "${dc}"
rm -f comp.bash

# The desktop board is a second binary, so the suite checks it was built and
# that it answers without opening a window — a headless runner can't do more
# than that, and this is what catches `riteui` disappearing from Cargo.toml.
# A CLI-only build (`cargo build -p rite-cli`) has no riteui, and that's not a
# failure.
echo "versions read each other's boards (ticket 193):"
cmp_repo="$(mktemp -d)"
cp -r "${ROOT}/tests/fixtures/board-4.5.0/." "${cmp_repo}/"
rm -f "${cmp_repo}/README.md"
( cd "${cmp_repo}" && git init -q -b main && git config user.email t@t.co && git config user.name t \
  && git config commit.gpgsign false && git add -A && git commit -q -m "a board written by 4.5.0" )
# Both names: the previous release may be a 4.x amd, which reads only AMD_ (ticket 309).
in_cmp() { (cd "${cmp_repo}" && RITE_NO_BRANCH=1 AMD_NO_BRANCH=1 RITE_SYNC=0 AMD_SYNC=0 XDG_CONFIG_HOME="${cmp_repo}.config" "$@"); }
assert "this build reads a board 4.5.0 wrote: every ticket, epic, sprint, link and repeat" \
  bash -c "$(declare -f in_cmp); cmp_repo='${cmp_repo}'; out=\$(in_cmp '${AMD}' board --plain) \
    && for id in 001 002 003 004 005 006 009; do printf '%s' \"\$out\" | grep -q \"\\[\$id\\]\" || exit 1; done \
    && in_cmp '${AMD}' checkout | grep -q '007' && in_cmp '${AMD}' s1 2>/dev/null | grep -q '008' \
    && in_cmp '${AMD}' links 4 | grep -q '002' && in_cmp '${AMD}' show 9 | grep -q 'recurs-from: \"003\"'"
assert "...and changes it: moves, a new ticket, a field" \
  bash -c "$(declare -f in_cmp); cmp_repo='${cmp_repo}'; in_cmp '${AMD}' doing 1 >/dev/null \
    && in_cmp '${AMD}' done 2 >/dev/null && in_cmp '${AMD}' new 'Written by the new build' --due 2031-01-01 --no-edit >/dev/null 2>&1 \
    && in_cmp '${AMD}' set 4 priority high >/dev/null && ls '${cmp_repo}'/tasks/backlog/010-written-by-the-new-build.md >/dev/null"
assert "reading the board changes nothing (no migration on open)" \
  bash -c "$(declare -f in_cmp); cmp_repo='${cmp_repo}'; before=\$(git -C '${cmp_repo}' rev-parse HEAD) \
    && in_cmp '${AMD}' board --plain >/dev/null && in_cmp '${AMD}' show 4 >/dev/null 2>&1 \
    && [ \"\$(git -C '${cmp_repo}' rev-parse HEAD)\" = \"\$before\" ] && [ -z \"\$(git -C '${cmp_repo}' status --porcelain)\" ]"
if [ -n "${RITE_PREVIOUS:-}" ] && [ -x "${RITE_PREVIOUS}" ]; then
  assert "the previous release ($("${RITE_PREVIOUS}" --version)) reads and changes the board this build wrote" \
    bash -c "$(declare -f in_cmp); cmp_repo='${cmp_repo}'; in_cmp '${RITE_PREVIOUS}' board --plain | grep -q 'Written by the new build' \
      && in_cmp '${RITE_PREVIOUS}' show 10 | grep -q 'due: \"2031-01-01\"' \
      && in_cmp '${RITE_PREVIOUS}' todo 10 --no-branch >/dev/null && ls '${cmp_repo}'/tasks/todo/010-written-by-the-new-build.md >/dev/null"
else
  echo "  skip - the previous release reading this build's board (set RITE_PREVIOUS to its rite; CI does)"
fi
assert "rite require-version refuses one above this build, and needs --yes without a terminal" \
  bash -c "$(declare -f in_cmp); cmp_repo='${cmp_repo}'; ! in_cmp '${AMD}' require-version 999.0.0 2>&1 | grep -q 'needs' \
    && in_cmp '${AMD}' require-version 999.0.0 2>&1 | grep -q 'update first' \
    && in_cmp '${AMD}' require-version 1.0.0 2>&1 | grep -q 'Add --yes to confirm' \
    && ! grep -q 'min-version' '${cmp_repo}'/.settings"
assert "with --yes it's set and committed, and says who it affects" \
  bash -c "$(declare -f in_cmp); cmp_repo='${cmp_repo}'; in_cmp '${AMD}' require-version 1.0.0 --yes | grep -q 'needs KanRite 1.0.0 or newer' \
    && grep -qx 'min-version = 1.0.0' '${cmp_repo}'/.settings && git -C '${cmp_repo}' log -1 --format=%s | grep -q 'min-version'"
# A project requiring a version newer than this build: as an older build
# would see it.
sed -i 's/^min-version = .*/min-version = 999.0.0/' "${cmp_repo}/.settings"
assert "a project needing a newer KanRite is read-only here, naming rite update" \
  bash -c "$(declare -f in_cmp); cmp_repo='${cmp_repo}'; in_cmp '${AMD}' board --plain >/dev/null \
    && ! in_cmp '${AMD}' new 'Refused' --no-edit 2>/tmp/amd-cmp.err && grep -q 'needs KanRite 999.0.0 or newer' /tmp/amd-cmp.err \
    && grep -q 'rite update' /tmp/amd-cmp.err && ! in_cmp '${AMD}' doing 4 2>/dev/null \
    && in_cmp '${AMD}' require-version | grep -q '999.0.0'"
rm -rf "${cmp_repo}" "${cmp_repo}.config" /tmp/amd-cmp.err

echo "rite update (ticket 191):"
upd="$(mktemp -d)"
mkdir -p "${upd}/bin" "${upd}/rel" "${upd}/plain" "${upd}/project/tasks/backlog"
cp "${AMD}" "${upd}/bin/rite"
printf '#!/usr/bin/env bash\n# agile-md install: release\n' > "${upd}/bin/rite-uninstall"
cp "${AMD}" "${upd}/plain/rite"
printf -- '---\nid: "001"\n---\n' > "${upd}/project/tasks/backlog/001-x.md"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) upd_target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64) upd_target=aarch64-unknown-linux-gnu ;;
  Darwin-x86_64) upd_target=x86_64-apple-darwin ;;
  *) upd_target=aarch64-apple-darwin ;;
esac
# A pretend 9.9.9: rite and riteui that only say their version, zipped as the
# release workflow does, and the release list naming its checksum.
python3 - "${upd}/rel/amd-${upd_target}.zip" <<'PYZIP'
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w") as z:
    for name in ("rite", "riteui"):
        info = zipfile.ZipInfo(name)
        info.external_attr = 0o100755 << 16
        z.writestr(info, '#!/bin/sh\necho "%s 9.9.9"\n' % name)
PYZIP
upd_sha="$(sha256sum "${upd}/rel/amd-${upd_target}.zip" | cut -d' ' -f1)"
upd_list() { # digest -> the release list
  printf '[{"tag_name":"v9.9.9","html_url":"https://example.test/9.9.9","draft":false,"prerelease":false,"assets":[{"name":"amd-%s.zip","browser_download_url":"file://%s/rel/amd-%s.zip","digest":"sha256:%s"}]}]' \
    "${upd_target}" "${upd}" "${upd_target}" "$1"
}
upd_list "$(printf '0%.0s' $(seq 64))" > "${upd}/rel/bad.json"
upd_list "${upd_sha}" > "${upd}/rel/releases.json"
upd_project="$(cd "${upd}/project" && find . -type f -exec sha256sum {} + | sort)"
assert "rite update --check says what's out" \
  bash -c "RITE_UPDATE_RELEASES='file://${upd}/rel/releases.json' '${upd}/bin/rite' update --check | grep -q 'KanRite 9.9.9 is out'"
assert "rite update refuses an install the installer didn't make, and says how" \
  bash -c "! RITE_UPDATE_RELEASES='file://${upd}/rel/releases.json' '${upd}/plain/rite' update 2>/tmp/amd-upd.err \
    && grep -q \"wasn't put there by KanRite's installer\" /tmp/amd-upd.err && '${upd}/plain/rite' --version | grep -qv 9.9.9"
assert "a checksum that doesn't match changes nothing" \
  bash -c "! RITE_UPDATE_RELEASES='file://${upd}/rel/bad.json' '${upd}/bin/rite' update >/dev/null 2>/tmp/amd-upd.err \
    && grep -q \"doesn't match its checksum\" /tmp/amd-upd.err && ! test -e '${upd}/bin/rite.previous' \
    && '${upd}/bin/rite' --version | grep -qv 9.9.9"
assert "rite update installs the release, keeps the one before, and leaves the project byte-identical" \
  bash -c "RITE_UPDATE_RELEASES='file://${upd}/rel/releases.json' '${upd}/bin/rite' update | grep -q 'rite is now 9.9.9' \
    && '${upd}/bin/rite' --version | grep -qx 'rite 9.9.9' && '${upd}/bin/riteui' --version | grep -qx 'riteui 9.9.9' \
    && test -x '${upd}/bin/rite.previous' \
    && [ \"\$(cd '${upd}/project' && find . -type f -exec sha256sum {} + | sort)\" = '${upd_project}' ]"
assert "rite update --rollback puts the previous rite back" \
  bash -c "'${upd}/bin/rite.previous' update --rollback | grep -q 'put back the previous rite' \
    && '${upd}/bin/rite' --version | grep -qv 9.9.9 && ! test -e '${upd}/bin/rite.previous'"
rm -rf "${upd}" /tmp/amd-upd.err

echo "a 4.x install updating across the rename (ticket 311):"
if [ -n "${RITE_PREVIOUS:-}" ] && "${RITE_PREVIOUS}" --version 2>/dev/null | grep -q '^amd 4\.'; then
  x4="$(mktemp -d)"
  mkdir -p "${x4}/bin" "${x4}/rel"
  # A 4.x release install: the real previous release's amd, an amdui, and
  # the uninstaller 4.x's install.sh wrote.
  cp "${RITE_PREVIOUS}" "${x4}/bin/amd"
  printf '#!/bin/sh\necho "amdui 4.9.0"\n' > "${x4}/bin/amdui"
  chmod +x "${x4}/bin/amdui"
  printf '#!/usr/bin/env bash\n# agile-md install: release\nfor name in amd amdui; do\n  [ -e "${BIN}/${name}" ] || continue\n  gone\ndone\n' > "${x4}/bin/amd-uninstall"
  # A release zip shaped like 5.x's: the new programs, and the same under
  # the 4.x names, which are what 4.x takes.
  python3 - "${x4}/rel/amd-${upd_target}.zip" <<'PYZIP'
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w") as z:
    for name, says in (("rite", "rite"), ("riteui", "riteui"), ("amd", "rite"), ("amdui", "riteui")):
        info = zipfile.ZipInfo(name)
        info.external_attr = 0o100755 << 16
        z.writestr(info, '#!/bin/sh\necho "%s 9.9.9"\n' % says)
PYZIP
  x4_sha="$(sha256sum "${x4}/rel/amd-${upd_target}.zip" | cut -d' ' -f1)"
  printf '[{"tag_name":"v9.9.9","html_url":"https://example.test/9.9.9","draft":false,"prerelease":false,"assets":[{"name":"amd-%s.zip","browser_download_url":"file://%s/rel/amd-%s.zip","digest":"sha256:%s"}]}]' \
    "${upd_target}" "${x4}" "${upd_target}" "${x4_sha}" > "${x4}/rel/releases.json"
  assert "the previous release's amd update takes the new programs by its old names" \
    bash -c "AMD_UPDATE_RELEASES='file://${x4}/rel/releases.json' '${x4}/bin/amd' update >/dev/null 2>&1 \
      && '${x4}/bin/amd' --version | grep -qx 'rite 9.9.9' && '${x4}/bin/amdui' --version | grep -qx 'riteui 9.9.9' \
      && '${x4}/bin/amd.previous' --version | grep -q '^amd 4\.' && ! test -e '${x4}/bin/rite'"
  # What lands is this build: the first run by the old name finishes the job.
  cp "${AMD}" "${x4}/bin/amd"
  assert "the first run of the new amd puts in rite and riteui, keeping amd and amdui as links" \
    bash -c "XDG_STATE_HOME='${x4}/state' '${x4}/bin/amd' --version 2>'${x4}/err' | grep -q '^rite ' \
      && grep -q 'finished updating from amd: rite and riteui now in' '${x4}/err' \
      && test -L '${x4}/bin/amd' && test -L '${x4}/bin/amdui' && test -f '${x4}/bin/rite' \
      && '${x4}/bin/riteui' --version | grep -qx 'riteui 9.9.9' \
      && grep -q 'for name in rite riteui amd amdui' '${x4}/bin/amd-uninstall'"
  assert "rite update --rollback puts the previous release back as amd, as it was" \
    bash -c "'${x4}/bin/rite' update --rollback | grep -q 'put back the previous amd and amdui' \
      && '${x4}/bin/amd' --version | grep -q '^amd 4\.' && ! test -L '${x4}/bin/amd' \
      && ! test -e '${x4}/bin/rite' && ! test -e '${x4}/bin/riteui'"
  rm -rf "${x4}"
else
  echo "  skip - a 4.x install updating across the rename (set RITE_PREVIOUS to a 4.x amd; CI does)"
fi

echo "install.sh over a 4.x install, and its uninstaller (ticket 312):"
ins="$(mktemp -d)"
mkdir -p "${ins}/bin" "${ins}/home/.local/share/applications" "${ins}/home/.local/share/icons/hicolor/scalable/apps"
# What a 4.x install left: amd, amdui, amd-uninstall and amdui's launcher.
printf '#!/bin/sh\necho "amd 4.9.0"\n' > "${ins}/bin/amd"
printf '#!/bin/sh\necho "amdui 4.9.0"\n' > "${ins}/bin/amdui"
printf '#!/usr/bin/env bash\n# agile-md install: release\n' > "${ins}/bin/amd-uninstall"
chmod +x "${ins}/bin/amd" "${ins}/bin/amdui" "${ins}/bin/amd-uninstall"
printf '[Desktop Entry]\nName=agile-md\n' > "${ins}/home/.local/share/applications/amdui.desktop"
printf '<svg/>' > "${ins}/home/.local/share/icons/hicolor/scalable/apps/amdui.svg"
python3 - "${ins}/release.zip" "${AMD}" "${AMDUI_BIN:-${ROOT}/target/debug/riteui}" <<'PYZIP'
import os, sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w") as z:
    for name, path in (("rite", sys.argv[2]), ("riteui", sys.argv[3])):
        if os.path.exists(path):
            info = zipfile.ZipInfo(name)
            info.external_attr = 0o100755 << 16
            with open(path, "rb") as f:
                z.writestr(info, f.read())
PYZIP
in_ins() { HOME="${ins}/home" XDG_DATA_HOME="${ins}/home/.local/share" XDG_CONFIG_HOME="${ins}/home/.config" \
  XDG_STATE_HOME="${ins}/home/.local/state" "$@"; }
assert "install.sh puts in rite, riteui, the amd and amdui links and rite-uninstall, and takes away 4.x's" \
  bash -c "$(declare -f in_ins); ins='${ins}'; in_ins bash '${ROOT}/install.sh' --dir '${ins}/bin' --zip '${ins}/release.zip' >/dev/null 2>&1 \
    && '${ins}/bin/rite' --version | grep -q '^rite ' && test -L '${ins}/bin/amd' && [ \"\$(readlink '${ins}/bin/amd')\" = rite ] \
    && grep -qx '# agile-md install: release' '${ins}/bin/rite-uninstall' && ! test -e '${ins}/bin/amd-uninstall'"
# install.sh installs the launcher on Linux only: no .desktop files elsewhere.
if [ "$(uname -s)" = "Linux" ] && [ -x "${AMDUI_BIN:-${ROOT}/target/debug/riteui}" ]; then
  assert "...and one launcher, riteui's: 4.x's amdui.desktop and its icon are gone" \
    bash -c "test -L '${ins}/bin/amdui' && test -f '${ins}/home/.local/share/applications/riteui.desktop' \
      && ! test -e '${ins}/home/.local/share/applications/amdui.desktop' \
      && ! test -e '${ins}/home/.local/share/icons/hicolor/scalable/apps/amdui.svg'"
fi
touch "${ins}/bin/rite.previous" "${ins}/bin/amd.previous"
assert "rite-uninstall removes the new names and the old, the kept copies and itself" \
  bash -c "$(declare -f in_ins); ins='${ins}'; in_ins '${ins}/bin/rite-uninstall' --yes >/dev/null 2>&1 \
    && [ -z \"\$(ls -A '${ins}/bin')\" ] && ! test -e '${ins}/home/.local/share/applications/riteui.desktop'"
rm -rf "${ins}"

echo "the machine folders move from agile-md to kanrite (ticket 310):"
mv_home="$(mktemp -d)"
mkdir -p "${mv_home}/config/agile-md" "${mv_home}/state/agile-md" "${mv_home}/data/agile-md/speech"
printf '/srv/clones/p-tickets\n' > "${mv_home}/config/agile-md/repos"
printf '{}' > "${mv_home}/state/agile-md/update.json"
in_mv() { XDG_CONFIG_HOME="${mv_home}/config" XDG_STATE_HOME="${mv_home}/state" XDG_DATA_HOME="${mv_home}/data" "$@"; }
assert "the first run moves each agile-md folder to kanrite, saying so, and leaves a link for older versions" \
  bash -c "$(declare -f in_mv); mv_home='${mv_home}'; in_mv '${AMD}' --version 2>'${mv_home}/err' >/dev/null \
    && [ \"\$(grep -c '^moved .*agile-md to .*kanrite' '${mv_home}/err')\" -eq 3 ] \
    && for d in config state data; do test -d '${mv_home}'/\$d/kanrite && [ \"\$(readlink '${mv_home}'/\$d/agile-md)\" = kanrite ] || exit 1; done \
    && grep -qx /srv/clones/p-tickets '${mv_home}/config/kanrite/repos' \
    && grep -qx /srv/clones/p-tickets '${mv_home}/config/agile-md/repos'"
assert "...once: the next run moves nothing" \
  bash -c "$(declare -f in_mv); mv_home='${mv_home}'; [ -z \"\$(in_mv '${AMD}' --version 2>&1 >/dev/null)\" ]"
rm -rf "${mv_home}"

echo "the old names, amd and amdui (ticket 309):"
old="$(mktemp -d)"
ln -s "${AMD}" "${old}/amd"
assert "amd, a symlink to rite, runs it and says once a day that it's now rite" \
  bash -c "XDG_STATE_HOME='${old}/state' '${old}/amd' --version 2>'${old}/err' | grep -q '^rite ' \
    && grep -q '^amd is now rite' '${old}/err' \
    && XDG_STATE_HOME='${old}/state' '${old}/amd' --version 2>'${old}/err' >/dev/null && ! test -s '${old}/err'"
assert "rite itself says nothing about the old name" \
  bash -c "[ -z \"\$(XDG_STATE_HOME='${old}/fresh' '${AMD}' --version 2>&1 >/dev/null)\" ]"
assert "an AMD_ variable from 4.x still counts, and RITE_ wins over it" \
  bash -c "cd '${nongit}' && AMD_DIR=olddir '${AMD}' board 2>&1 | grep -q '<repo>/olddir' \
    && AMD_DIR=olddir RITE_DIR=newdir '${AMD}' board 2>&1 | grep -q '<repo>/newdir'"
rm -rf "${old}"

echo "the desktop binary:"
AMDUI="${AMDUI_BIN:-${ROOT}/target/debug/riteui}"
if [ -x "${AMDUI}" ]; then
  assert "riteui --version names itself" \
    bash -c "'${AMDUI}' --version | grep -q '^riteui '"
  assert "riteui --help mentions the board" \
    bash -c "'${AMDUI}' --help | grep -q 'desktop board'"
  assert "riteui rejects an unknown argument" \
    bash -c "! '${AMDUI}' --nope"
else
  echo "  skip - no riteui binary (built without the gui feature)"
fi

# The terminal board is gone (ticket 082): no subcommand for it.
assert "rite has no tui subcommand" \
  bash -c "! '${AMD}' tui </dev/null >/dev/null 2>&1 && ! '${AMD}' --help | grep -q 'terminal board'"

echo "guards:"
assert "errors outside a git repository" \
  bash -c "cd '${nongit}' && ! '${AMD}' board"

echo "non-interactive (--no-input / RITE_NO_INPUT):"
assert "new with no title errors instead of prompting" \
  bash -c "! '${AMD}' new"
assert "new with no title shows the usage line" \
  bash -c "'${AMD}' new 2>&1 | grep -q 'usage: rite new'"
assert "rite todo with no ref lists instead of prompting, even non-interactively" \
  bash -c "'${AMD}' todo >/dev/null"
assert "--no-input works without the env var" \
  bash -c "env -u RITE_NO_INPUT '${AMD}' --no-input show 2>&1 | grep -q 'usage: rite show <ref>'"

echo "rite mcp, the board for AI assistants (ticket 186):"
mcp="$(mktemp -d)"
(
  cd "${mcp}" && git init -q board && cd board && git config user.email t@t.co && git config user.name Tess
  RITE_YES=1 "${AMD}" init mcpdemo --local-only >/dev/null && git add -A && git commit -q -m "board for ticket 186"
)
in_mcp() { (cd "${mcp}/board" && XDG_STATE_HOME="${mcp}/state" "${AMD}" mcp 2>"${mcp}/stderr"); }
rpc() { # id tool json-arguments
  printf '{"jsonrpc":"2.0","id":%s,"method":"tools/call","params":{"name":"%s","arguments":%s}}\n' "$1" "$2" "$3"
}
{
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}'
  printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  rpc 3 create_ticket '{"title":"Make the widget","priority":"high","points":"3","body":"## Notes\n\nFrom the assistant.\n"}'
  rpc 4 move_ticket '{"ticket":"1","to":"doing"}'
  rpc 5 move_ticket '{"ticket":"1","to":"todo"}'
  rpc 6 add_note '{"ticket":"1","text":"Checked the sizes."}'
  rpc 7 board '{}'
} | in_mcp > "${mcp}/out"
assert "rite mcp writes nothing but JSON-RPC on stdout, one reply per request" \
  bash -c "[ \"\$(wc -l < '${mcp}/out')\" -eq 7 ] && ! grep -v '\"jsonrpc\":\"2.0\"' '${mcp}/out' | grep -q . && ! grep -qv '^{' '${mcp}/out'"
assert "initialize agrees the protocol version and names the server" \
  bash -c "grep '\"id\":1,' '${mcp}/out' | grep -q '\"protocolVersion\":\"2025-06-18\"' && grep '\"id\":1,' '${mcp}/out' | grep -q '\"name\":\"kanrite\"'"
assert "tools/list offers the read and change tools, and nothing that abandons" \
  bash -c "l=\$(grep '\"id\":2,' '${mcp}/out') && for t in screen board ticket search_tickets create_ticket move_ticket edit_ticket add_note file_ticket; do printf '%s' \"\$l\" | grep -q \"\\\"name\\\":\\\"\$t\\\"\" || exit 1; done && ! printf '%s' \"\$l\" | grep -q abandon"
assert "create_ticket makes a backlog ticket with its priority and points, committed" \
  bash -c "grep -q 'priority: \"high\"' '${mcp}'/board/tasks/*/001-make-the-widget.md && git -C '${mcp}/board' log --format=%s | grep -qx 'new: 001 \"Make the widget\"'"
assert "a move the board refuses comes back as a tool error saying why" \
  bash -c "grep '\"id\":4,' '${mcp}/out' | grep -q '\"isError\":true' && grep '\"id\":4,' '${mcp}/out' | grep -q 'one column at a time'"
assert "move_ticket moves it as rite todo does, and commits" \
  bash -c "test -f '${mcp}/board/tasks/todo/001-make-the-widget.md' && git -C '${mcp}/board' log --format=%s | grep -qx 'todo: 001 \"Make the widget\"'"
assert "add_note signs and dates a note under ## Notes" \
  grep -q '^\*\*Tess, 20[0-9-]*:\*\* Checked the sizes\.$' "${mcp}/board/tasks/todo/001-make-the-widget.md"
assert "board lists the columns with the ticket in todo" \
  bash -c "grep '\"id\":7,' '${mcp}/out' | grep -q 'board#001 Make the widget'"
assert "git's own output goes to stderr, never into the protocol" \
  grep -q 'new: 001' "${mcp}/stderr"
assert "screen says riteui isn't open on the project when no window has written a note" \
  bash -c "rpc() { printf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"method\":\"tools/call\",\"params\":{\"name\":\"%s\",\"arguments\":%s}}\\n' \"\$1\" \"\$2\" \"\$3\"; }
    out=\$(rpc 1 screen '{}' | (cd '${mcp}/board' && XDG_STATE_HOME='${mcp}/state' '${AMD}' mcp 2>/dev/null)) \
    && printf '%s' \"\$out\" | grep -q \"riteui isn't open\""
assert "rite mcp setup prints how to connect an AI tool" \
  bash -c "'${AMD}' mcp setup claude | grep -qx '  claude mcp add --scope user kanrite -- rite mcp' && '${AMD}' mcp setup | grep -q Codex"
oldmcp="$(mktemp -d)"
printf '{"mcpServers":{"agile-md":{}}}' > "${oldmcp}/.claude.json"
assert "rite mcp setup says when a tool still holds the old agile-md entry" \
  bash -c "HOME='${oldmcp}' '${AMD}' mcp setup claude | grep -q 'Found the old agile-md entry in: claude'"
rm -f "${oldmcp}/.claude.json"
assert "rite mcp setup --fix changes nothing when there is no old entry" \
  bash -c "HOME='${oldmcp}' '${AMD}' mcp setup --fix | grep -q 'No agile-md entry'"
rm -rf "${oldmcp}"
assert "an archived project's tickets can't be changed through rite mcp" \
  bash -c "cd '${mcp}/board' && echo 'archived = on' >> .settings && out=\$(printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/call\",\"params\":{\"name\":\"create_ticket\",\"arguments\":{\"title\":\"No\"}}}' | '${AMD}' mcp 2>/dev/null) && printf '%s' \"\$out\" | grep -q '\"isError\":true' && printf '%s' \"\$out\" | grep -q archived"
rm -rf "${mcp}"

echo "RITE_DIR:"
# A fresh repo, not `${tmp}`: README/settings now live at the repository
# root, so a second RITE_DIR-relocated board inside the same, already-heavily
# -used `${tmp}` repo would share (and inherit the counter from) the
# existing `tasks/` board's settings file — an edge case nobody hits in
# practice (one repo picks one board location), not what this checks.
dir_repo="$(mktemp -d)"
( cd "${dir_repo}" && git init -q && git config user.email t@t.co && git config user.name t )
assert "RITE_DIR relocates the board" \
  bash -c "cd '${dir_repo}' && RITE_DIR=work RITE_YES=1 '${AMD}' new 'Elsewhere' >/dev/null && test -f work/backlog/001-elsewhere.md"
# Left behind, this repo becomes the one board next to the next run's fresh
# repo in /tmp, and Board::discover() rightly finds it.
rm -rf "${dir_repo}"

echo "auto-create when no board exists:"
fresh="$(mktemp -d)"; ( cd "${fresh}" && git init -q )
assert "non-interactive with no board errors (no hang)" \
  bash -c "cd '${fresh}' && ! '${AMD}' board </dev/null"
assert "RITE_YES=1 creates the board and runs" \
  bash -c "cd '${fresh}' && RITE_YES=1 '${AMD}' board >/dev/null && test -d tasks/todo"
rm -rf "${fresh}"

echo
echo "passed: ${pass}, failed: ${fail}"
[ "${fail}" -eq 0 ]
