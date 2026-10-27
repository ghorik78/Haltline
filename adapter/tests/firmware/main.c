/* Test firmware for the Haltline end-to-end tests (QEMU netduinoplus2,
 * STM32F405RG). Lines tagged with `@name` are looked up by the tests. */
#include <stdint.h>

struct point {
    int x;
    int y;
};

volatile uint32_t g_counter = 0;
struct point g_point = {3, 4};
uint8_t g_buffer[16];
uint32_t g_big[300];
float g_ratio = 1.5f;
volatile int g_trigger_fault = 0;
static int s_calls = 0;

#define TIM2_BASE 0x40000000u
#define TIM2_PSC (*(volatile uint32_t *)(TIM2_BASE + 0x28u))
#define TIM2_ARR (*(volatile uint32_t *)(TIM2_BASE + 0x2Cu))

static void update_point(struct point *p, int dx)
{
    p->x += dx; /* @update_x */
    p->y -= dx;
    s_calls++;
}

int factorial(int n)
{
    if (n <= 1) {
        return 1; /* @fact_base */
    }
    return n * factorial(n - 1);
}

static void fault_now(void)
{
    __asm volatile("udf #0"); /* @fault */
}

int main(void)
{
    TIM2_PSC = 7; /* @main_first */
    TIM2_ARR = 0x1234;
    g_buffer[0] = 0xAB;
    for (int i = 0; i < 10; i++) {
        g_counter += i; /* @loop */
        update_point(&g_point, i); /* @call_update */
    }
    int f = factorial(5); /* @call_fact */
    g_ratio = g_ratio * 2.0f;
    g_big[299] = (uint32_t)f;
    if (g_trigger_fault) {
        fault_now();
    }
    while (1) {
        g_counter++; /* @idle */
    }
}
