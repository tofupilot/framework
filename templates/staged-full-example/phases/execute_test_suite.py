import sys
import time


def execute_test_suite(test_api, ui):
    print(f"Executing test suite for unit {test_api.slot_id}...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Test suite completed for unit {test_api.slot_id}", file=sys.stderr)
    return "CONTINUE"
