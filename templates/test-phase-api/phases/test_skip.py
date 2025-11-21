def test_skip_with_message(phase, run):
    """Test that phase.skip() correctly skips with a message."""
    run.log.info("This phase will be skipped")
    phase.skip("Skipping this phase for testing")
