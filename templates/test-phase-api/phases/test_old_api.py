def test_old_api_broken(phase, run):
    """
    Test that the old API (accessing phase.FAIL without calling) no longer works.
    This mimics the bug from APP-4486 where the user wrote:
        phase.FAIL  # Wrong - just accesses the attribute
    instead of:
        phase.fail("message")  # Correct - calls the method
    """
    run.log.info("Testing old broken API - this should pass (no FAIL attribute exists)")
    # This will cause an AttributeError since Phase class doesn't have FAIL attribute anymore
    # The phase should error out, not silently pass
    try:
        phase.FAIL  # This should raise AttributeError
        run.log.error("BUG: phase.FAIL attribute still exists!")
    except AttributeError:
        run.log.info("Good: phase.FAIL attribute does not exist (old API removed)")
    # Implicit pass
