import java.math.BigInteger;

public class ClockTestBigIntegerWarm {
    // Same volatile-sink safeguard as ClockTestLongWarm, for consistency --
    // less likely to matter here (BigInteger's per-call overhead is much
    // harder for the JIT to reduce to a closed form), but doesn't hurt.
    static volatile BigInteger sink;

    static long runOnce() {
        long start = System.nanoTime();
        BigInteger acc = BigInteger.ZERO;
        for (int i = 0; i < 300000; i++) {
            acc = acc.add(BigInteger.ONE);
        }
        sink = acc;
        long end = System.nanoTime();
        return end - start;
    }

    public static void main(String[] args) {
        int runs = 20;
        for (int r = 0; r < runs; r++) {
            long ns = runOnce();
            System.out.println("run " + r + ": elapsed = " + (ns / 1_000_000_000.0) + " seconds");
        }
    }
}
