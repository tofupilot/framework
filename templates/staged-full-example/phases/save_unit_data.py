import sys
import time


def save_unit_data(test_api, ui):
    print(f"Saving data for unit {test_api.slot_id} (runs for each unit)...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Data saved for unit {test_api.slot_id}", file=sys.stderr)
    return "CONTINUE"
