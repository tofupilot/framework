import sys
import time


def initialize_test(run, ui):
    print("Initializing test environment...", file=sys.stderr)
    time.sleep(0.5)
    print("Test environment initialized successfully", file=sys.stderr)
