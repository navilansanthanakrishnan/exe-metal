// dx12bench — a small DirectX 12 workload used to check whether the D3D12 path
// works at all under Wine + D3DMetal, and how much throughput survives.
//
// Two numbers matter and they fail for different reasons:
//   --draws N   issues N separate draw calls per frame. This is API/CPU bound,
//               which is where Wine and Rosetta cost you.
//   --iters N   makes the pixel shader more expensive. This is GPU bound, which
//               is where D3DMetal's translation quality shows up.
//
// Reports the adapter D3D12 hands back, so it also confirms which driver is
// actually answering.

#define WIN32_LEAN_AND_MEAN
#define INITGUID
#include <windows.h>
#include <d3d12.h>
#include <dxgi1_4.h>
#include <d3dcompiler.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>

static const char *kShader = R"HLSL(
cbuffer Params : register(b0) { float4 gP; };   // x: phase, y: shader iterations

struct VSOut { float4 pos : SV_POSITION; float2 uv : TEXCOORD0; };

VSOut VSMain(uint vid : SV_VertexID)
{
    // One oversized triangle; the scissor keeps it on screen.
    float2 p = float2((vid << 1) & 2, vid & 2);
    VSOut o;
    o.uv  = p;
    o.pos = float4(p * 2.0 - 1.0, 0, 1);
    return o;
}

float4 PSMain(VSOut i) : SV_TARGET
{
    float3 c = float3(i.uv, frac(gP.x));
    int n = (int)gP.y;
    [loop] for (int k = 0; k < n; ++k) {
        c = frac(c * 1.6180339 + float3(0.1, 0.2, 0.3));
        c = sqrt(abs(c) + 1e-3);
    }
    return float4(c, 1);
}
)HLSL";

static const UINT kFrames = 2;

static LRESULT CALLBACK WndProc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    if (m == WM_DESTROY) { PostQuitMessage(0); return 0; }
    return DefWindowProcW(h, m, w, l);
}

#define CHECK(hr, what)                                                        \
    do {                                                                       \
        HRESULT _hr = (hr);                                                    \
        if (FAILED(_hr)) {                                                     \
            printf("FAIL  %s -> 0x%08lX\n", what, (unsigned long)_hr);         \
            return 2;                                                          \
        }                                                                      \
    } while (0)

int main(int argc, char **argv)
{
    int width = 1920, height = 1080, draws = 1, iters = 64, seconds = 10;
    for (int i = 1; i < argc; ++i) {
        if (!strcmp(argv[i], "--width")   && i + 1 < argc) width   = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--height") && i + 1 < argc) height = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--draws")  && i + 1 < argc) draws  = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--iters")  && i + 1 < argc) iters  = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--seconds")&& i + 1 < argc) seconds= atoi(argv[++i]);
    }
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("dx12bench  %dx%d  draws/frame=%d  shader-iters=%d  duration=%ds\n",
           width, height, draws, iters, seconds);

    // --- adapter -----------------------------------------------------------
    IDXGIFactory4 *factory = NULL;
    CHECK(CreateDXGIFactory1(IID_PPV_ARGS(&factory)), "CreateDXGIFactory1");

    ID3D12Device *dev = NULL;
    IDXGIAdapter1 *adapter = NULL;
    for (UINT i = 0; factory->EnumAdapters1(i, &adapter) != DXGI_ERROR_NOT_FOUND; ++i) {
        DXGI_ADAPTER_DESC1 d;
        adapter->GetDesc1(&d);
        if (d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) { adapter->Release(); continue; }
        if (SUCCEEDED(D3D12CreateDevice((IUnknown *)adapter, D3D_FEATURE_LEVEL_11_0,
                                        IID_PPV_ARGS(&dev)))) {
            printf("adapter    %S\n", d.Description);
            printf("vram       %llu MB\n",
                   (unsigned long long)(d.DedicatedVideoMemory >> 20));
            break;
        }
        adapter->Release();
        adapter = NULL;
    }
    if (!dev) { printf("FAIL  no D3D12 device — DirectX 12 is not available\n"); return 2; }

    D3D_FEATURE_LEVEL levels[] = { D3D_FEATURE_LEVEL_12_2, D3D_FEATURE_LEVEL_12_1,
                                   D3D_FEATURE_LEVEL_12_0, D3D_FEATURE_LEVEL_11_1,
                                   D3D_FEATURE_LEVEL_11_0 };
    D3D12_FEATURE_DATA_FEATURE_LEVELS fl = { 5, levels, D3D_FEATURE_LEVEL_11_0 };
    if (SUCCEEDED(dev->CheckFeatureSupport(D3D12_FEATURE_FEATURE_LEVELS, &fl, sizeof(fl))))
        printf("feature    level %X_%X\n", (fl.MaxSupportedFeatureLevel >> 12) & 0xF,
               (fl.MaxSupportedFeatureLevel >> 8) & 0xF);

    D3D12_FEATURE_DATA_D3D12_OPTIONS5 o5 = {};
    if (SUCCEEDED(dev->CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS5, &o5, sizeof(o5))))
        printf("raytracing %s\n", o5.RaytracingTier ? "yes" : "no");

    // --- window and swap chain --------------------------------------------
    WNDCLASSEXW wc = {};
    wc.cbSize = sizeof(wc);
    wc.lpfnWndProc = WndProc;
    wc.hInstance = GetModuleHandleW(NULL);
    wc.lpszClassName = L"dx12bench";
    RegisterClassExW(&wc);
    RECT rc = { 0, 0, width, height };
    AdjustWindowRect(&rc, WS_OVERLAPPEDWINDOW, FALSE);
    HWND hwnd = CreateWindowExW(0, L"dx12bench", L"dx12bench", WS_OVERLAPPEDWINDOW,
                                CW_USEDEFAULT, CW_USEDEFAULT, rc.right - rc.left,
                                rc.bottom - rc.top, NULL, NULL, wc.hInstance, NULL);
    if (!hwnd) { printf("FAIL  CreateWindow\n"); return 2; }
    ShowWindow(hwnd, SW_SHOW);

    ID3D12CommandQueue *queue = NULL;
    D3D12_COMMAND_QUEUE_DESC qd = {};
    qd.Type = D3D12_COMMAND_LIST_TYPE_DIRECT;
    CHECK(dev->CreateCommandQueue(&qd, IID_PPV_ARGS(&queue)), "CreateCommandQueue");

    DXGI_SWAP_CHAIN_DESC1 sd = {};
    sd.BufferCount = kFrames;
    sd.Width = width;
    sd.Height = height;
    sd.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD;
    sd.SampleDesc.Count = 1;

    IDXGISwapChain1 *sc1 = NULL;
    CHECK(factory->CreateSwapChainForHwnd((IUnknown *)queue, hwnd, &sd, NULL, NULL, &sc1),
          "CreateSwapChainForHwnd");
    IDXGISwapChain3 *swap = NULL;
    CHECK(sc1->QueryInterface(IID_PPV_ARGS(&swap)), "QueryInterface SwapChain3");

    ID3D12DescriptorHeap *rtvHeap = NULL;
    D3D12_DESCRIPTOR_HEAP_DESC hd = {};
    hd.NumDescriptors = kFrames;
    hd.Type = D3D12_DESCRIPTOR_HEAP_TYPE_RTV;
    CHECK(dev->CreateDescriptorHeap(&hd, IID_PPV_ARGS(&rtvHeap)), "CreateDescriptorHeap");
    UINT rtvSize = dev->GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV);

    ID3D12Resource *targets[kFrames] = {};
    D3D12_CPU_DESCRIPTOR_HANDLE rtv = rtvHeap->GetCPUDescriptorHandleForHeapStart();
    for (UINT i = 0; i < kFrames; ++i) {
        CHECK(swap->GetBuffer(i, IID_PPV_ARGS(&targets[i])), "GetBuffer");
        dev->CreateRenderTargetView(targets[i], NULL, rtv);
        rtv.ptr += rtvSize;
    }

    // --- pipeline ----------------------------------------------------------
    ID3D12RootSignature *root = NULL;
    {
        D3D12_ROOT_PARAMETER p = {};
        p.ParameterType = D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS;
        p.Constants.Num32BitValues = 4;
        p.ShaderVisibility = D3D12_SHADER_VISIBILITY_ALL;
        D3D12_ROOT_SIGNATURE_DESC rsd = {};
        rsd.NumParameters = 1;
        rsd.pParameters = &p;
        rsd.Flags = D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT;
        ID3DBlob *sig = NULL, *err = NULL;
        CHECK(D3D12SerializeRootSignature(&rsd, D3D_ROOT_SIGNATURE_VERSION_1, &sig, &err),
              "D3D12SerializeRootSignature");
        CHECK(dev->CreateRootSignature(0, sig->GetBufferPointer(), sig->GetBufferSize(),
                                       IID_PPV_ARGS(&root)), "CreateRootSignature");
    }

    ID3DBlob *vs = NULL, *ps = NULL, *err = NULL;
    if (FAILED(D3DCompile(kShader, strlen(kShader), NULL, NULL, NULL, "VSMain", "vs_5_0",
                          0, 0, &vs, &err))) {
        printf("FAIL  vertex shader: %s\n", err ? (char *)err->GetBufferPointer() : "?");
        return 2;
    }
    if (FAILED(D3DCompile(kShader, strlen(kShader), NULL, NULL, NULL, "PSMain", "ps_5_0",
                          0, 0, &ps, &err))) {
        printf("FAIL  pixel shader: %s\n", err ? (char *)err->GetBufferPointer() : "?");
        return 2;
    }

    ID3D12PipelineState *pso = NULL;
    {
        D3D12_GRAPHICS_PIPELINE_STATE_DESC pd = {};
        pd.pRootSignature = root;
        pd.VS = { vs->GetBufferPointer(), vs->GetBufferSize() };
        pd.PS = { ps->GetBufferPointer(), ps->GetBufferSize() };
        pd.RasterizerState.FillMode = D3D12_FILL_MODE_SOLID;
        pd.RasterizerState.CullMode = D3D12_CULL_MODE_NONE;
        pd.RasterizerState.DepthClipEnable = TRUE;
        for (int i = 0; i < 8; ++i)
            pd.BlendState.RenderTarget[i].RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL;
        pd.SampleMask = UINT_MAX;
        pd.PrimitiveTopologyType = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
        pd.NumRenderTargets = 1;
        pd.RTVFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
        pd.SampleDesc.Count = 1;
        CHECK(dev->CreateGraphicsPipelineState(&pd, IID_PPV_ARGS(&pso)),
              "CreateGraphicsPipelineState");
    }

    ID3D12CommandAllocator *alloc = NULL;
    ID3D12GraphicsCommandList *cl = NULL;
    CHECK(dev->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT, IID_PPV_ARGS(&alloc)),
          "CreateCommandAllocator");
    CHECK(dev->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, alloc, pso,
                                 IID_PPV_ARGS(&cl)), "CreateCommandList");
    cl->Close();

    ID3D12Fence *fence = NULL;
    CHECK(dev->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(&fence)), "CreateFence");
    UINT64 fenceVal = 0;
    HANDLE fenceEvent = CreateEventW(NULL, FALSE, FALSE, NULL);

    D3D12_VIEWPORT vp = { 0, 0, (FLOAT)width, (FLOAT)height, 0, 1 };
    D3D12_RECT scissor = { 0, 0, width, height };

    printf("rendering...\n");

    LARGE_INTEGER freq, t0, now;
    QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&t0);
    double limit = (double)seconds;
    UINT64 frames = 0;
    double worst = 0.0;
    LARGE_INTEGER prev = t0;

    for (;;) {
        MSG msg;
        while (PeekMessageW(&msg, NULL, 0, 0, PM_REMOVE)) {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if (msg.message == WM_QUIT) goto done;
        }

        UINT idx = swap->GetCurrentBackBufferIndex();
        alloc->Reset();
        cl->Reset(alloc, pso);

        D3D12_RESOURCE_BARRIER b = {};
        b.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
        b.Transition.pResource = targets[idx];
        b.Transition.StateBefore = D3D12_RESOURCE_STATE_PRESENT;
        b.Transition.StateAfter = D3D12_RESOURCE_STATE_RENDER_TARGET;
        b.Transition.Subresource = D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
        cl->ResourceBarrier(1, &b);

        D3D12_CPU_DESCRIPTOR_HANDLE h = rtvHeap->GetCPUDescriptorHandleForHeapStart();
        h.ptr += idx * rtvSize;
        const FLOAT clear[4] = { 0.02f, 0.02f, 0.06f, 1.0f };
        cl->OMSetRenderTargets(1, &h, FALSE, NULL);
        cl->ClearRenderTargetView(h, clear, 0, NULL);
        cl->RSSetViewports(1, &vp);
        cl->RSSetScissorRects(1, &scissor);
        cl->SetGraphicsRootSignature(root);
        cl->IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

        for (int d = 0; d < draws; ++d) {
            float params[4] = { (float)frames * 0.01f + d * 0.001f, (float)iters, 0, 0 };
            cl->SetGraphicsRoot32BitConstants(0, 4, params, 0);
            cl->DrawInstanced(3, 1, 0, 0);
        }

        b.Transition.StateBefore = D3D12_RESOURCE_STATE_RENDER_TARGET;
        b.Transition.StateAfter = D3D12_RESOURCE_STATE_PRESENT;
        cl->ResourceBarrier(1, &b);
        cl->Close();

        ID3D12CommandList *lists[] = { cl };
        queue->ExecuteCommandLists(1, lists);
        swap->Present(0, 0);

        // Serialising every frame keeps the measurement honest: without it the
        // CPU races ahead and reports submission rate, not frame rate.
        const UINT64 target = ++fenceVal;
        queue->Signal(fence, target);
        if (fence->GetCompletedValue() < target) {
            fence->SetEventOnCompletion(target, fenceEvent);
            WaitForSingleObject(fenceEvent, INFINITE);
        }

        ++frames;
        QueryPerformanceCounter(&now);
        double dt = (double)(now.QuadPart - prev.QuadPart) / freq.QuadPart;
        prev = now;
        if (frames > 10 && dt > worst) worst = dt;

        double elapsed = (double)(now.QuadPart - t0.QuadPart) / freq.QuadPart;
        if (elapsed >= limit) break;
    }

done:
    QueryPerformanceCounter(&now);
    double total = (double)(now.QuadPart - t0.QuadPart) / freq.QuadPart;
    printf("\nRESULT\n");
    printf("  frames        %llu\n", (unsigned long long)frames);
    printf("  elapsed       %.2f s\n", total);
    printf("  average fps   %.1f\n", frames / total);
    printf("  frame time    %.2f ms\n", 1000.0 * total / (double)frames);
    printf("  worst frame   %.2f ms\n", worst * 1000.0);
    printf("  draw calls/s  %.0f\n", frames * (double)draws / total);
    return 0;
}
