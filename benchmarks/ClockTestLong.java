public class ClockTestLong {
    public static void main(String[] args) {
        long start = System.nanoTime();
        long acc = 0;
        for (int i = 0; i < 300000; i++) {
            acc += 1;
        }
        long end = System.nanoTime();
        double elapsed = (end - start) / 1_000_000_000.0;
        System.out.println("elapsed = " + elapsed + " seconds (acc=" + acc + ")");
    }
}
