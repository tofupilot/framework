"""Phase control flow examples showing different return types."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def do_nothing(phase, run, ui):
    """Returns None = CONTINUE, outcome is PASS/FAIL based on measurements."""
    time.sleep(0.25)
    pass


def return_continue(phase, run, ui):
    """Returns CONTINUE, outcome is PASS/FAIL based on measurements."""
    time.sleep(0.25)
    


def return_true(phase, run, ui):
    """Returns True = CONTINUE (pass), outcome is PASS."""
    time.sleep(0.25)
    return True


def return_false(phase, run, ui):
    """Returns False = FAIL, outcome is FAIL."""
    time.sleep(0.25)
    return False


def return_fail(phase, run, ui):
    """Returns FAIL, outcome is FAIL (when.fail determines next action)."""
    time.sleep(0.25)
    phase.fail("Phase failed")


def measurement_validation_fail(phase, run, measurements, ui):
    """Returns CONTINUE but measurement fails critical validator → outcome is FAIL."""
    time.sleep(0.25)
    measurements.voltage = 2.5
    


def return_skip(phase, run, ui):
    """Returns SKIP, outcome is SKIP, measurements ignored."""
    time.sleep(0.25)
    phase.skip("Skipping phase")


def return_repeat(phase, run, ui):
    """Returns RETRY up to retry_limit times (additional attempts after initial)."""
    time.sleep(0.25)
    phase.retry("Retrying phase")


def conditional_repeat(phase, run, ui):
    """Retries until the last attempt, then passes."""
    time.sleep(0.25)
    if run.retry_count >= run.retry_limit:

    else:
        phase.retry("Retrying phase")


def slow_phase(phase, run, ui):
    """Sleeps for 5 seconds - will timeout with 2s timeout."""
    time.sleep(5)
    


def raise_error(phase, run, ui):
    """Raises an exception to test when.error handling."""
    raise RuntimeError("Simulated error for when.error testing")


def return_stop(phase, run, ui):
    """Returns STOP to halt execution immediately, outcome is ERROR."""
    time.sleep(0.25)
    phase.stop("Stopping execution")


def never_executed_phase(phase, run, ui):
    """Should never execute because previous phase returned STOP."""
    print("ERROR: This phase should never execute!", file=sys.stderr)
    time.sleep(0.25)
    
