/* Runs a microWakeWord model on frontend features (rows of 40 values, as the fixtures store
   them) with TFLite Micro and prints the raw uint8 output per 3-step group. */
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <math.h>
void* ww_create(const uint8_t* model);
int ww_invoke(void* h, const int8_t* in);
int main(int argc, char** argv) {
  FILE* f = fopen(argv[1], "rb"); fseek(f, 0, SEEK_END); long n = ftell(f); rewind(f);
  uint8_t* model = aligned_alloc(16, (n + 15) / 16 * 16); fread(model, 1, n, f); fclose(f);
  void* h = ww_create(model); if (!h) { fprintf(stderr, "create failed\n"); return 1; }
  const double scale = atof(argv[2]); const int zp = atoi(argv[3]);
  FILE* in = fopen(argv[4], "r"); int8_t buf[120]; int k = 0; unsigned v;
  while (fscanf(in, "%u", &v) == 1) {
    double x = v * 0.0390625 / scale + zp;
    double r = nearbyint(x); /* round half to even, as numpy */
    if (r < -128) r = -128; if (r > 127) r = 127;
    buf[k++] = (int8_t)r;
    if (k == 120) { printf("%d\n", ww_invoke(h, buf)); k = 0; }
  }
  return 0;
}
