import sys
import time


def initialize_system(test_api, ui):
    print("Initializing entire test system (runs once)...", file=sys.stderr)
    time.sleep(0.5)
    print("Test system initialized", file=sys.stderr)
    return "CONTINUE"
