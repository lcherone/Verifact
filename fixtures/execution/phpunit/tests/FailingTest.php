<?php
use PHPUnit\Framework\TestCase;
final class FailingTest extends TestCase
{
    public function testAssertionFailsVisibly(): void { self::assertSame(5, 2 + 2); }
}
