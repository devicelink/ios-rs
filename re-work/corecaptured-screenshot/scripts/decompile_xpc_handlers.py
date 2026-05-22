# Ghidra script to decompile XPC handler functions in CoreCaptureDaemon
import ghidra.app.decompiler.DecompInterface as DecompInterface

decomp = DecompInterface()
decomp.openProgram(currentProgram)
fm = currentProgram.getFunctionManager()

# Target functions based on the symbols we found
target_keywords = [
    "remoteCaptureE",
    "eventHandler",
    "connectionHandler",
    "handleIncomingMessage",
    "handleConnectionEvent",
    "listenE",
    "CCXPCRemote",
    "CCXPCService",
]

print("=== Decompiling XPC Handler Functions ===")

for func in fm.getFunctions(True):
    name = func.getName()
    should_decompile = any(kw in name for kw in target_keywords)
    if should_decompile:
        print("\n=== FUNCTION: %s @ %s ===" % (name, func.getEntryPoint()))
        result = decomp.decompileFunction(func, 120, None)
        if result and result.getDecompiledFunction():
            print(result.getDecompiledFunction().getC())
        else:
            print("  [Decompilation failed]")

decomp.dispose()
