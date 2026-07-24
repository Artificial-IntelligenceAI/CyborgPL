public class ClockTestLongWarm {
    // volatile forces the JIT to actually perform every write rather than
    // proving the loop reduces to a closed-form constant and eliminating
    // it -- the first version of this file didn't have this, and the
    // warmed JIT-compiled loop dropped to ~0 seconds once it noticed the
    // whole loop was dead code (same issue black_box fixes in Rust).
    static volatile long sink;

    static long runOnce() {
        long start = System.nanoTime();
        long acc = 0;
        for (int i = 0; i < 300000; i++) {
            acc += 1;
            sink = acc;
        }
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
