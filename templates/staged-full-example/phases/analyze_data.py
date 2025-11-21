import sys
import time


def analyze_data(test_api, ui):
    print(f"Analyzing data for unit {test_api.slot_id}...", file=sys.stderr)
    time.sleep(0.5)
    print(f"Data analysis completed for unit {test_api.slot_id}", file=sys.stderr)
    return "CONTINUE"
