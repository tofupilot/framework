import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_auto_evaluate(phase, run, ui):
    """YAML validator with outcome=UNSET for auto-evaluation"""

    # Provide value that will be auto-evaluated against YAML validator (>= 3.0)
    # Randomly choose a value that will sometimes pass, sometimes fail
    voltage = random.choice([2.5, 3.5])  # 2.5 will fail >= 3.0, 3.5 will pass
    measurements.auto_evaluate_test = voltage
    run.log.info(f"Auto-evaluate test voltage: {voltage}V (will be auto-evaluated against >= 3.0)")

    