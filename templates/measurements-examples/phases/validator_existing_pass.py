import sys
import os
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_existing_pass(phase, run, ui):
    """YAML validator with outcome=PASS"""

    # Provide value that matches the YAML expected value (3.3V)
    # YAML validator has outcome=PASS, so it should always pass regardless of value
    voltage = 3.3
    measurements.existing_pass_test = voltage
    run.log.info(f"Existing pass test voltage: {voltage}V")

    