#!/bin/bash
set -e
PRJ="$(dirname "$(readlink -f "$0")")"
BUILD="$PRJ/build"

cmake -B "$BUILD" -S "$PRJ" \
  $@

cd tests_lua

$BUILD/lua all.lua
