# Sourced by the release builds (scripts/*/build.sh): the API clients built
# into them, so signing in to AniList and MyAnimeList needs no setup. They're
# read from a file kept outside the repository,
#   ~/.local/share/ichioshi-release/keys.env (ICHIOSHI_KEYS: another file),
# of lines like
#   ICHIOSHI_ANILIST_CLIENT_ID='...'
#   ICHIOSHI_ANILIST_CLIENT_SECRET='...'
#   ICHIOSHI_MAL_CLIENT_ID='...'
#   ICHIOSHI_MAL_CLIENT_SECRET='...'   (only if MyAnimeList gave one)
# Without it, a build has none: signing in asks for your own client.
keys=${ICHIOSHI_KEYS:-$HOME/.local/share/ichioshi-release/keys.env}
if [ -f "$keys" ]; then
    set -a
    # shellcheck disable=SC1090
    . "$keys"
    set +a
    # Whether each is there (never what it is).
    has() { if [ -n "$1" ]; then echo yes; else echo no; fi; }
    echo "API clients built in (from $keys): AniList $(has "${ICHIOSHI_ANILIST_CLIENT_SECRET:-}"), MyAnimeList $(has "${ICHIOSHI_MAL_CLIENT_ID:-}")" >&2
else
    echo "No $keys: this build has no API clients of its own" >&2
fi
