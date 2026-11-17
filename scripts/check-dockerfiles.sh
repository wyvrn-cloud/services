#!/bin/sh
# Every service's Dockerfile begins with the same shared build (from "The shared
# build" to "end of the shared build"), word for word: Docker builds it once only if
# it is the same in each. This says where one differs.
set -e
cd "$(dirname "$0")/.."
base() { sed -n '/^# The shared build/,/end of the shared build/p' "$1"; }
first=""
status=0
for file in services/*/Dockerfile; do
    if [ -z "$first" ]; then first=$file; continue; fi
    if [ "$(base "$first")" != "$(base "$file")" ]; then
        echo "$file's shared build differs from $first's:"
        base "$first" > /tmp/base-a.$$; base "$file" > /tmp/base-b.$$
        diff -u /tmp/base-a.$$ /tmp/base-b.$$ || true
        rm -f /tmp/base-a.$$ /tmp/base-b.$$
        status=1
    fi
done
[ -n "$(base "$first")" ] || { echo "$first has no shared build"; exit 1; }
[ $status -eq 0 ] && echo "The shared build is the same in every service's Dockerfile."
exit $status
