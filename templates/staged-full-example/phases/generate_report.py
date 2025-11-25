import sys
import time


def generate_report(run, ui):
    print("Generating final test report (runs once)...", file=sys.stderr)
    time.sleep(0.5)
    print("Final report generated", file=sys.stderr)
