<?php
use PHPUnit\Framework\TestCase;
final class ArithmeticTest extends TestCase
{
    public function testAddition(): void { self::assertSame(4, 2 + 2); }
    public function testSubtraction(): void { self::assertSame(0, 2 - 2); }
}
