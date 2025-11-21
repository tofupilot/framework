"""Phase control flow examples showing different return types."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def do_nothing(phase, test_api, ui):
    """Returns None = CONTINUE, outcome is PASS/FAIL based on measurements."""
    time.sleep(0.25)
    pass


def return_continue(phase, test_api, ui):
    """Returns CONTINUE, outcome is PASS/FAIL based on measurements."""
    time.sleep(0.25)
    


def return_true(phase, test_api, ui):
    """Returns True = CONTINUE (pass), outcome is PASS."""
    time.sleep(0.25)
    return True


def return_false(phase, test_api, ui):
    """Returns False = FAIL, outcome is FAIL."""
    time.sleep(0.25)
    return False


def return_fail(phase, test_api, ui):
    """Returns FAIL, outcome is FAIL (when.fail determines next action)."""
    time.sleep(0.25)
    phase.fail("Phase failed")


def measurement_validation_fail(phase, test_api, ui):
    """Returns CONTINUE but measurement fails critical validator → outcome is FAIL."""
    time.sleep(0.25)
    test_api.measurements.voltage = 2.5
    


def return_skip(phase, test_api, ui):
    """Returns SKIP, outcome is SKIP, measurements ignored."""
    time.sleep(0.25)
    phase.skip("Skipping phase")


def return_repeat(phase, test_api, ui):
    """Returns RETRY up to retry_limit times (additional attempts after initial)."""
    time.sleep(0.25)
    phase.retry("Retrying phase")


def conditional_repeat(phase, test_api, ui):
    """Retries until the last attempt, then passes."""
    time.sleep(0.25)
    if test_api.retry_count >= test_api.retry_limit - 1:
        
    else:
        phase.retry("Retrying phase")


def slow_phase(phase, test_api, ui):
    """Sleeps for 5 seconds - will timeout with 2s timeout."""
    time.sleep(5)
    


def raise_error(phase, test_api, ui):
    """Raises an exception to test when.error handling."""
    raise RuntimeError("Simulated error for when.error testing")


def return_stop(phase, test_api, ui):
    """Returns STOP to halt execution immediately, outcome is ERROR."""
    time.sleep(0.25)
    phase.stop("Stopping execution")


def never_executed_phase(phase, test_api, ui):
    """Should never execute because previous phase returned STOP."""
    print("ERROR: This phase should never execute!", file=sys.stderr)
    time.sleep(0.25)
    
