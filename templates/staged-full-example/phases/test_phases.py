import sys
import time


def initialize_system(test_api, ui):
    print("Initializing entire test system (runs once)...", file=sys.stderr)
    time.sleep(0.5)
    print("Test system initialized", file=sys.stderr)
    return "CONTINUE"


def calibrate_equipment(test_api, ui):
    print("Calibrating shared test equipment (runs once)...", file=sys.stderr)
    time.sleep(0.5)
    print("Equipment calibrated", file=sys.stderr)
    return "CONTINUE"


def prepare_unit(test_api, ui):
    print(f"Preparing unit {test_api.slot_id} (runs for each unit)...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Unit {test_api.slot_id} prepared", file=sys.stderr)
    return "CONTINUE"


def verify_unit_ready(test_api, ui):
    print(f"Verifying unit {test_api.slot_id} is ready...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Unit {test_api.slot_id} is ready", file=sys.stderr)
    return "CONTINUE"


def execute_test_suite(test_api, ui):
    print(f"Executing test suite for unit {test_api.slot_id}...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Test suite completed for unit {test_api.slot_id}", file=sys.stderr)
    return "CONTINUE"


def collect_measurements(test_api, ui):
    print(f"Collecting measurements for unit {test_api.slot_id}...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Measurements collected for unit {test_api.slot_id}", file=sys.stderr)
    return "CONTINUE"


def analyze_data(test_api, ui):
    print(f"Analyzing data for unit {test_api.slot_id}...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Data analysis completed for unit {test_api.slot_id}", file=sys.stderr)
    return "CONTINUE"


def save_unit_data(test_api, ui):
    print(f"Saving data for unit {test_api.slot_id} (runs for each unit)...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Data saved for unit {test_api.slot_id}", file=sys.stderr)
    return "CONTINUE"


def reset_unit(test_api, ui):
    print(f"Resetting unit {test_api.slot_id} (runs for each unit)...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Unit {test_api.slot_id} reset", file=sys.stderr)
    return "CONTINUE"


def generate_report(test_api, ui):
    print("Generating final test report (runs once)...", file=sys.stderr)
    time.sleep(0.5)
    print("Final report generated", file=sys.stderr)
    return "CONTINUE"


def shutdown_system(test_api, ui):
    print("Shutting down test system (runs once)...", file=sys.stderr)
    time.sleep(0.5)
    print("Test system shut down", file=sys.stderr)
    return "CONTINUE"
