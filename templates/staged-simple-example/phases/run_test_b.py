import sys
import time


def run_test_b(test_api, ui):
    print("Running test scenario B...", file=sys.stderr)
    time.sleep(0.5)
    print("Test B completed", file=sys.stderr)
    return "CONTINUE"
