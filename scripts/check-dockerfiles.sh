#!/bin/sh
# Every service's Dockerfile begins with the same shared base (from "The shared
# base" to "end of the shared base"), word for word: Docker builds it once only if
# it is the same in each. This says where one differs.
set -e
cd "$(dirname "$0")/.."
base() { sed -n '/^# The shared base/,/end of the shared base/p' "$1"; }
first=""
status=0
for file in services/*/Dockerfile; do
    if [ -z "$first" ]; then first=$file; continue; fi
    if [ "$(base "$first")" != "$(base "$file")" ]; then
        echo "$file's shared base differs from $first's:"
        base "$first" > /tmp/base-a.$$; base "$file" > /tmp/base-b.$$
        diff -u /tmp/base-a.$$ /tmp/base-b.$$ || true
        rm -f /tmp/base-a.$$ /tmp/base-b.$$
        status=1
    fi
done
[ -n "$(base "$first")" ] || { echo "$first has no shared base"; exit 1; }
[ $status -eq 0 ] && echo "The shared base is the same in every service's Dockerfile."
exit $status
