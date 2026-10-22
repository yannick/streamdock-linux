// extern "C" shim: what a Rust FFI would bind to.
#include <new>
#include <cstdint>
#include <cstring>
#include "tensorflow/lite/micro/micro_interpreter.h"
#include "tensorflow/lite/micro/micro_mutable_op_resolver.h"
#include "tensorflow/lite/micro/micro_allocator.h"
#include "tensorflow/lite/micro/micro_resource_variable.h"

namespace {
constexpr size_t kArena = 40 * 1024;
struct Ww {
  alignas(16) uint8_t arena[kArena];
  tflite::MicroMutableOpResolver<13> resolver;
  alignas(tflite::MicroInterpreter) uint8_t interp_buf[sizeof(tflite::MicroInterpreter)];
  tflite::MicroInterpreter* interp = nullptr;
};
}

extern "C" {
// model must outlive the handle. Returns nullptr on error.
void* ww_create(const uint8_t* model_data) {
  const tflite::Model* model = tflite::GetModel(model_data);
  if (model->version() != TFLITE_SCHEMA_VERSION) return nullptr;
  Ww* w = new (std::nothrow) Ww();
  if (!w) return nullptr;
  auto& r = w->resolver;
  if (r.AddCallOnce() || r.AddVarHandle() || r.AddReadVariable() || r.AddAssignVariable() ||
      r.AddStridedSlice() || r.AddConcatenation() || r.AddConv2D() || r.AddDepthwiseConv2D() ||
      r.AddSplitV() || r.AddReshape() || r.AddFullyConnected() || r.AddLogistic() || r.AddQuantize()) {
    delete w; return nullptr;
  }
  tflite::MicroAllocator* alloc = tflite::MicroAllocator::Create(w->arena, kArena);
  tflite::MicroResourceVariables* vars = tflite::MicroResourceVariables::Create(alloc, 6);
  w->interp = new (w->interp_buf) tflite::MicroInterpreter(model, r, alloc, vars);
  if (w->interp->AllocateTensors() != kTfLiteOk) { w->interp->~MicroInterpreter(); delete w; return nullptr; }
  return w;
}
size_t ww_input_len(void* h) { return static_cast<Ww*>(h)->interp->input(0)->bytes; }
size_t ww_arena_used(void* h) { return static_cast<Ww*>(h)->interp->arena_used_bytes(); }
int ww_input_type(void* h) { return static_cast<Ww*>(h)->interp->input(0)->type; }
int ww_output_type(void* h) { return static_cast<Ww*>(h)->interp->output(0)->type; }
// input: ww_input_len bytes (int8 features). Returns output byte (uint8 prob) or -1.
int ww_invoke(void* h, const int8_t* in) {
  Ww* w = static_cast<Ww*>(h);
  TfLiteTensor* t = w->interp->input(0);
  std::memcpy(t->data.int8, in, t->bytes);
  if (w->interp->Invoke() != kTfLiteOk) return -1;
  return w->interp->output(0)->data.uint8[0];
}
void ww_destroy(void* h) {
  Ww* w = static_cast<Ww*>(h);
  w->interp->~MicroInterpreter();
  delete w;
}
}
