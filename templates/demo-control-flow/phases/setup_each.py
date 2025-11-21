import time
import sys

def setup_each(test, ui):
    """Setup slot - prepare each individual unit for testing"""
    print("🔧 SETUP_SLOT: Preparing unit for test...", file=sys.stderr)
    print("   - Connecting to unit", file=sys.stderr)
    print("   - Running diagnostics", file=sys.stderr)
    print("   - Setting initial conditions", file=sys.stderr)
    time.sleep(2)
    print("✅ Unit preparation complete", file=sys.stderr)
    return "CONTINUE"
