import sys
import time


def initialize_test(run, ui):
    print("Initializing test environment...", file=sys.stderr)
    time.sleep(0.5)
    print("Test environment initialized successfully", file=sys.stderr)


def run_measurement(run, ui):
    print("Running measurement...", file=sys.stderr)
    time.sleep(0.5)
    print("Measurement completed", file=sys.stderr)


def validate_results(run, ui):
    print("Validating results...", file=sys.stderr)
    time.sleep(0.5)
    print("Results validated successfully", file=sys.stderr)
