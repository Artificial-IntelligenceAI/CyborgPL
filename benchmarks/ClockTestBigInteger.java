import java.math.BigInteger;

public class ClockTestBigInteger {
    public static void main(String[] args) {
        long start = System.nanoTime();
        BigInteger acc = BigInteger.ZERO;
        for (int i = 0; i < 300000; i++) {
            acc = acc.add(BigInteger.ONE);
        }
        long end = System.nanoTime();
        double elapsed = (end - start) / 1_000_000_000.0;
        System.out.println("elapsed = " + elapsed + " seconds (acc=" + acc + ")");
    }
}
