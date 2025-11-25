import sys
import time


def prepare_environment(ui):
    print("Preparing environment for unit...", file=sys.stderr)
    time.sleep(0.5)
    print("Environment prepared", file=sys.stderr)
