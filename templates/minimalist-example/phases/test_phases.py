import sys
import time


def initialize_test(test_api, ui):
    print("Initializing test environment...", file=sys.stderr)
    time.sleep(0.5)
    print("Test environment initialized successfully", file=sys.stderr)
    return "CONTINUE"


def run_measurement(test_api, ui):
    print("Running measurement...", file=sys.stderr)
    time.sleep(0.5)
    print("Measurement completed", file=sys.stderr)
    return "CONTINUE"


def validate_results(test_api, ui):
    print("Validating results...", file=sys.stderr)
    time.sleep(0.5)
    print("Results validated successfully", file=sys.stderr)
    return "CONTINUE"
