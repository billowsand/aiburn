#!/bin/sh
# Tests for is903burn.
#
# Part 1 runs against a mock IS903 device, so it works without hardware.
# Part 2 runs against real hardware and needs the device plugged in.

set -e
cd "$(dirname "$0")/.."

rc=0
pass() { echo "  PASS: $1"; }
fail() { echo "  FAIL: $1"; rc=1; }

echo "== building =="
make >/dev/null
gcc -shared -fPIC -o tests/mock_is903.so tests/mock_is903.c -ldl
pass "build"

echo
echo "== mock device tests =="
: > /tmp/mockdev
export LD_PRELOAD="$PWD/tests/mock_is903.so"
export IS903_SG=/tmp/mockdev

if ./is903burn list | grep -q "IS903"; then
	pass "list finds the device"
else
	fail "list finds the device"
fi

if ./is903burn info | grep -q "P.M."; then
	pass "info lists the command groups"
else
	fail "info lists the command groups"
fi

./is903burn read 0 64 /tmp/mock-read.bin >/dev/null
if [ "$(stat -c %s /tmp/mock-read.bin)" = 32768 ]; then
	pass "read transfers the requested blocks"
else
	fail "read transfers the requested blocks"
fi

dd if=/dev/urandom of=/tmp/mock-src.bin bs=1k count=100 2>/dev/null
rm -f /tmp/mock-write.bin
MOCK_WRITE_OUT=/tmp/mock-write.bin ./is903burn write 0 /tmp/mock-src.bin --force >/dev/null
if cmp -s /tmp/mock-src.bin /tmp/mock-write.bin; then
	pass "write round trips"
else
	fail "write round trips"
fi

if ./is903burn key --pcs 1 | grep -q "key accepted"; then
	pass "key exchange"
else
	fail "key exchange"
fi

if ./is903burn capacity >/dev/null 2>&1; then
	pass "capacity query"
else
	fail "capacity query"
fi

# block-device path used by burn/dump
if [ -f /tmp/current-iso-backup.bin ]; then
	cp /tmp/current-iso-backup.bin /tmp/blk-test.img
	export IS903_BLK=/tmp/blk-test.img
	if ./is903burn burn /tmp/current-iso-backup.bin --force >/dev/null 2>&1 \
	   && ./is903burn dump /tmp/blk-out.iso >/dev/null 2>&1 \
	   && cmp -s /tmp/current-iso-backup.bin /tmp/blk-out.iso; then
		pass "burn/dump round trip"
	else
		fail "burn/dump round trip"
	fi
else
	echo "  SKIP: burn/dump (no ISO fixture)"
fi

unset LD_PRELOAD IS903_SG IS903_BLK

echo
if [ $rc -eq 0 ]; then
	echo "all mock tests passed"
else
	echo "some mock tests FAILED"
fi
exit $rc
