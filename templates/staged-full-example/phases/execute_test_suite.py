import sys
import time


def execute_test_suite(run, ui):
    print(f"Executing test suite for unit {run.slot_id}...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Test suite completed for unit {run.slot_id}", file=sys.stderr)
