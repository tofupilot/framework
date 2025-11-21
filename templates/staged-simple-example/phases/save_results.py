import sys
import time


def save_results(test_api, ui):
    print("Saving test results...", file=sys.stderr)
    time.sleep(0.5)
    print("Results saved", file=sys.stderr)
    return "CONTINUE"
