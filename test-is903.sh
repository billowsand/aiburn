#!/bin/sh
# Smoke test for is903burn against a real IS903.
#
# Requires the sg module for /dev/sg0:   sudo modprobe sg
# and read/write access to /dev/sg0 and the block device:
#   sudo setfacl -m u:$USER:rw /dev/sg0 /dev/sdN

set -e
cd "$(dirname "$0")"

echo "== devices =="
./is903burn list

echo
echo "== identification (INQU) =="
./is903burn info

echo
echo "== disc capacity =="
./is903burn capacity

echo
echo "== P.M. status / info =="
./is903burn status

echo
echo "== extract the ISO image (dump) =="
./is903burn dump /tmp/is903-dump.iso
ls -l /tmp/is903-dump.iso
file /tmp/is903-dump.iso
