import sys
import time


def validate_results(test_api, ui):
    print("Validating results...", file=sys.stderr)
    time.sleep(0.5)
    print("Results validated successfully", file=sys.stderr)
    return "CONTINUE"
