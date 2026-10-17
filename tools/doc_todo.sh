#!/usr/bin/env bash
# What in safety_controller/src/ still lacks Doxygen comments.
#
# Doxygen documents nothing in a file until the file has an @file block, so
# files without one are listed first, then the undocumented members of the
# files that have one. The Doxyfile's EXTRACT_ALL = YES hides the second
# list from an ordinary build; this run turns it off.
#
#   tools/doc_todo.sh          # a few seconds; needs doxygen
#
# argus_wire.h and argus_replay_client.h are skipped: they are vendored from
# argus_core, so their comments are written there.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
command -v doxygen > /dev/null || {
    echo "doxygen not found: sudo apt install doxygen graphviz"
    exit 1
}

vendored='argus_wire\.h|argus_replay_client\.h'
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT

files=$(grep -L '@file' safety_controller/src/*.[ch] | grep -Ev "$vendored")
members=$( { cat Doxyfile
             echo "EXTRACT_ALL = NO"
             echo "HAVE_DOT = NO"
             echo "OUTPUT_DIRECTORY = $out"; } \
           | doxygen - 2>&1 \
           | grep 'is not documented' \
           | grep -Ev "$vendored" \
           | sed "s|^$PWD/||")

echo "--- files with no @file block yet"
printf '%s\n' "${files:-(none)}"
echo "--- undocumented members of files that have one"
printf '%s\n' "${members:-(none)}"
printf '\n%s file(s) to start, %s member(s) to document\n' \
    "$(printf '%s' "$files" | grep -c .)" "$(printf '%s' "$members" | grep -c .)"
