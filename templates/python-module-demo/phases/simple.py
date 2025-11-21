from tofupilot_runtime import TestContext

def simple(phase, context: TestContext):
    """Simple single-file module - function name matches filename"""
    context.log.info("Testing simple module: phases.simple → simple()")
    context.record_measurement("resistance", 100.0, "Ω")
    
