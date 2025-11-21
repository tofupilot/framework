def test_implicit_pass(phase, run):
    """Test that returning normally (no explicit return) passes the phase."""
    run.log.info("This phase will pass implicitly")
    run.measurements.add("voltage", 3.3, unit="V")
    # Implicit pass - just return normally
