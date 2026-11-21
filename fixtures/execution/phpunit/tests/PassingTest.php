<?php
use PHPUnit\Framework\TestCase;
final class PassingTest extends TestCase
{
    public function testAdditionPasses(): void { self::assertSame(4, 2 + 2); }
    public function testDeliberatelySkipped(): void { self::markTestSkipped('Synthetic skip'); }
}
