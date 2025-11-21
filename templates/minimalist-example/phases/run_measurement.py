import sys
import time


def run_measurement(test_api, ui):
    print("Running measurement...", file=sys.stderr)
    time.sleep(0.5)
    print("Measurement completed", file=sys.stderr)
    return "CONTINUE"
