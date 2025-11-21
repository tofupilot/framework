import sys
import time


def prepare_environment(test_api, ui):
    print("Preparing environment for unit...", file=sys.stderr)
    time.sleep(0.5)
    print("Environment prepared", file=sys.stderr)
    return "CONTINUE"


def load_configuration(test_api, ui):
    print("Loading test configuration...", file=sys.stderr)
    time.sleep(0.5)
    print("Configuration loaded", file=sys.stderr)
    return "CONTINUE"


def run_test_a(test_api, ui):
    print("Running test scenario A...", file=sys.stderr)
    time.sleep(0.5)
    print("Test A completed", file=sys.stderr)
    return "CONTINUE"


def run_test_b(test_api, ui):
    print("Running test scenario B...", file=sys.stderr)
    time.sleep(0.5)
    print("Test B completed", file=sys.stderr)
    return "CONTINUE"


def save_results(test_api, ui):
    print("Saving test results...", file=sys.stderr)
    time.sleep(0.5)
    print("Results saved", file=sys.stderr)
    return "CONTINUE"


def cleanup_environment(test_api, ui):
    print("Cleaning up environment...", file=sys.stderr)
    time.sleep(0.5)
    print("Environment cleaned up", file=sys.stderr)
    return "CONTINUE"
