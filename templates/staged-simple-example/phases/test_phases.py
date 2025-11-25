import sys
import time


def prepare_environment(ui):
    print("Preparing environment for unit...", file=sys.stderr)
    time.sleep(0.5)
    print("Environment prepared", file=sys.stderr)


def load_configuration(ui):
    print("Loading test configuration...", file=sys.stderr)
    time.sleep(0.5)
    print("Configuration loaded", file=sys.stderr)


def run_test_a(ui):
    print("Running test scenario A...", file=sys.stderr)
    time.sleep(0.5)
    print("Test A completed", file=sys.stderr)


def run_test_b(ui):
    print("Running test scenario B...", file=sys.stderr)
    time.sleep(0.5)
    print("Test B completed", file=sys.stderr)


def save_results(ui):
    print("Saving test results...", file=sys.stderr)
    time.sleep(0.5)
    print("Results saved", file=sys.stderr)


def cleanup_environment(ui):
    print("Cleaning up environment...", file=sys.stderr)
    time.sleep(0.5)
    print("Environment cleaned up", file=sys.stderr)
