import sys
import time


def calibrate_equipment(run, ui):
    print("Calibrating shared test equipment (runs once)...", file=sys.stderr)
    time.sleep(0.5)
    print("Equipment calibrated", file=sys.stderr)
