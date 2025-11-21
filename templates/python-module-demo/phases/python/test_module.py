from tofupilot_runtime import TestContext

def test_module(phase, context: TestContext):
    """Function name matches module name - can omit function field"""
    context.log.info("Testing optional function field: function name = module name")
    context.record_measurement("voltage", 5.0, "V")
    

def explicit_function(phase, context: TestContext):
    """Function name differs from module - must specify function field"""
    context.log.info("Testing with explicit function field specified")
    context.record_measurement("current", 0.5, "A")
    
