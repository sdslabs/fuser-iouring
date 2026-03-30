#!/usr/bin/env bash

set -x

exit_handler() {
    exit "${TEST_EXIT_STATUS:-1}"
}
trap exit_handler TERM
trap 'kill $(jobs -p); exit $TEST_EXIT_STATUS' INT EXIT

export RUST_BACKTRACE=1

NC="\e[39m"
GREEN="\e[32m"
RED="\e[31m"

DATA_DIR=$(mktemp --directory)
DIR=$(mktemp --directory)

cargo build --example simple > /dev/null 2>&1

cargo run --example simple $DIR $DATA_DIR 2>&1 &
FUSE_PID=$!

sleep 2

echo "mounting at $DIR"
mount | grep fuse || exit 1

if touch $DIR/a && echo "test" > $DIR/a && cat $DIR/a > /dev/null && mkdir $DIR/b && ls $DIR > /dev/null && rm $DIR/a && rmdir $DIR/b; then
    echo -e "${GREEN} OK simplefs operations ${NC}"
else
    echo -e "${RED} FAILED simplefs operations ${NC}"
    export TEST_EXIT_STATUS=1
    exit 1
fi

umount $DIR

kill $FUSE_PID
wait $FUSE_PID

export TEST_EXIT_STATUS=0
