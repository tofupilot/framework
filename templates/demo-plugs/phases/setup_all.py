import time
import sys

def setup_all(phase, run, ui):
    """Setup procedure - initialize the entire test system once at start"""
    print("🚀 SETUP_PROCEDURE: Initializing test system...", file=sys.stderr)
    print("   - Starting test equipment", file=sys.stderr)
    print("   - Calibrating instruments", file=sys.stderr)
    print("   - Loading configuration", file=sys.stderr)
    time.sleep(2)
    print("✅ Test system initialized successfully", file=sys.stderr)
    
