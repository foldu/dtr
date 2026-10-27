#!/usr/bin/env bash
# A virtual serial port with a real shell on the far end.
#
#   ./scripts/dev-serial.sh            # creates /tmp/dtr-serial
#   cargo run -- --port /tmp/dtr-serial
#
# socat gives us a pty pair; bash runs on one side with a pty of its own, so
# readline, echo, colours and cursor movement all exercise the real code path
# bidirectionally — better than replaying a recorded byte stream.
set -euo pipefail

LINK=${1:-/tmp/dtr-serial}
rm -f "$LINK"

cleanup() {
    if [[ -n "${SOCAT_PID:-}" ]]; then
        kill "$SOCAT_PID" 2>/dev/null || true
        wait "$SOCAT_PID" 2>/dev/null || true
    fi
    rm -f "$LINK"
}
trap cleanup EXIT INT TERM

socat "pty,raw,echo=0,link=$LINK" EXEC:'bash -i',pty,raw,echo=0 &
SOCAT_PID=$!

for _ in $(seq 50); do
    [[ -e "$LINK" ]] && break
    sleep 0.05
done
[[ -e "$LINK" ]] || { echo "socat did not create $LINK" >&2; exit 1; }

echo "virtual serial port: $LINK"
echo "run: cargo run -- --port $LINK"
wait "$SOCAT_PID"
