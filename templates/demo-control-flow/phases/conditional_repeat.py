"""Phase control flow examples showing different return types."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def conditional_repeat(phase, run, ui):
    """Retries until the last attempt, then passes."""
    time.sleep(0.25)
    if run.retry_count >= run.retry_limit:

    else:
        phase.retry("Retrying phase")
