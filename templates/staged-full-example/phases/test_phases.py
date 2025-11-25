import sys
import time


def initialize_system(phase, run, ui):
    print("Initializing entire test system (runs once)...", file=sys.stderr)
    time.sleep(0.5)
    print("Test system initialized", file=sys.stderr)


def calibrate_equipment(phase, run, ui):
    print("Calibrating shared test equipment (runs once)...", file=sys.stderr)
    time.sleep(0.5)
    print("Equipment calibrated", file=sys.stderr)


def prepare_unit(phase, run, ui):
    print(f"Preparing unit {run.slot_id} (runs for each unit)...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Unit {run.slot_id} prepared", file=sys.stderr)


def verify_unit_ready(phase, run, ui):
    print(f"Verifying unit {run.slot_id} is ready...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Unit {run.slot_id} is ready", file=sys.stderr)


def execute_test_suite(phase, run, ui):
    print(f"Executing test suite for unit {run.slot_id}...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Test suite completed for unit {run.slot_id}", file=sys.stderr)


def collect_measurements(phase, run, ui):
    print(f"Collecting measurements for unit {run.slot_id}...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Measurements collected for unit {run.slot_id}", file=sys.stderr)


def analyze_data(phase, run, ui):
    print(f"Analyzing data for unit {run.slot_id}...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Data analysis completed for unit {run.slot_id}", file=sys.stderr)


def save_unit_data(phase, run, ui):
    print(f"Saving data for unit {run.slot_id} (runs for each unit)...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Data saved for unit {run.slot_id}", file=sys.stderr)


def reset_unit(phase, run, ui):
    print(f"Resetting unit {run.slot_id} (runs for each unit)...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Unit {run.slot_id} reset", file=sys.stderr)


def generate_report(phase, run, ui):
    print("Generating final test report (runs once)...", file=sys.stderr)
    time.sleep(0.5)
    print("Final report generated", file=sys.stderr)


def shutdown_system(phase, run, ui):
    print("Shutting down test system (runs once)...", file=sys.stderr)
    time.sleep(0.5)
    print("Test system shut down", file=sys.stderr)
