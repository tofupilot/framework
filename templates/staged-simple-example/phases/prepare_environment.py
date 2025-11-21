import sys
import time


def prepare_environment(test_api, ui):
    print("Preparing environment for unit...", file=sys.stderr)
    time.sleep(0.5)
    print("Environment prepared", file=sys.stderr)
    return "CONTINUE"
