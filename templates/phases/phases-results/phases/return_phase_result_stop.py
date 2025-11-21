"""Test STOP phase return value cases."""

import sys
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def return_phase_result_stop(phase):
    """Call phase.STOP → Stop (halts procedure execution)."""
    phase.stop("Stopping execution")
