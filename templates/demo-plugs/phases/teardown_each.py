import time
import sys

def teardown_each(phase, run, ui):
    """Cleanup slot - clean up after testing each unit"""
    print("🧹 CLEANUP_SLOT: Finalizing unit test...", file=sys.stderr)
    print("   - Saving test data", file=sys.stderr)
    print("   - Disconnecting from unit", file=sys.stderr)
    print("   - Resetting for next unit", file=sys.stderr)
    time.sleep(0.25)
    print("✅ Unit test finalized", file=sys.stderr)
    
