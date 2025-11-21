def test_fail_with_message(phase, run):
    """Test that phase.fail() correctly fails with a message."""
    run.log.info("This phase will fail with a message")
    phase.fail("This is a test failure message")
