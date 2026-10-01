use super::{ErosionConfig, HeightField, State};
use anyhow::{anyhow, bail, Result};
use cudarc::{
    driver::*,
    nvrtc::{compile_ptx_with_opts, CompileOptions},
};
use std::sync::{Arc, OnceLock};

#[repr(C)]
#[derive(Clone, Copy)]
struct Params {
    width: u32,
    height: u32,
    spacing: f32,
    dt: f32,
    rain: f32,
    conductance: f32,
    capacity: f32,
    erosion: f32,
    deposition: f32,
    max_erosion: f32,
}
// The CUDA definition below has the same C layout, with only 32-bit scalars.
unsafe impl DeviceRepr for Params {}

struct Runtime {
    context: Arc<CudaContext>,
    flux: CudaFunction,
    update: CudaFunction,
}
static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

pub(super) fn run(f: &HeightField, c: ErosionConfig) -> Result<State> {
    let runtime = RUNTIME
        .get_or_init(|| initialize().map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow!("{e}"))?;
    let stream = runtime.context.default_stream();
    let n = f.heights_m.len();
    let base = stream.clone_htod(&f.heights_m)?;
    let fixed = stream.clone_htod(&f.protected)?;
    let mut delta = stream.alloc_zeros::<f32>(n)?;
    let mut next_delta = stream.alloc_zeros::<f32>(n)?;
    let mut water = stream.alloc_zeros::<f32>(n)?;
    let mut next_water = stream.alloc_zeros::<f32>(n)?;
    let mut sediment = stream.alloc_zeros::<f32>(n)?;
    let mut next_sediment = stream.alloc_zeros::<f32>(n)?;
    let mut flux = stream.alloc_zeros::<f32>(n * 4)?;
    let mut sediment_flux = stream.alloc_zeros::<f32>(n * 4)?;
    let mut exported_water = stream.alloc_zeros::<f32>(n)?;
    let mut exported_sediment = stream.alloc_zeros::<f32>(n)?;
    let mut eroded = stream.alloc_zeros::<f32>(n)?;
    let mut deposited = stream.alloc_zeros::<f32>(n)?;
    let p = Params {
        width: f.width as u32,
        height: f.height as u32,
        spacing: f.spacing_m,
        dt: c.dt_s,
        rain: c.rainfall_m_per_step,
        conductance: super::conductance(f, c),
        capacity: c.capacity,
        erosion: c.erosion_rate_s,
        deposition: c.deposition_rate_s,
        max_erosion: c.max_erosion_m_per_step,
    };
    let launch = LaunchConfig::for_num_elems(n as u32);
    for _ in 0..c.steps {
        // SAFETY: each kernel checks its index; arrays have n (or 4*n) elements.
        // Old/new height, water and sediment buffers never alias, and launches
        // on the same stream enforce flux -> gather -> next-step ordering.
        unsafe {
            stream
                .launch_builder(&runtime.flux)
                .arg(&base)
                .arg(&fixed)
                .arg(&delta)
                .arg(&water)
                .arg(&sediment)
                .arg(&mut flux)
                .arg(&mut sediment_flux)
                .arg(&p)
                .launch(launch)?;
            stream
                .launch_builder(&runtime.update)
                .arg(&base)
                .arg(&fixed)
                .arg(&delta)
                .arg(&water)
                .arg(&sediment)
                .arg(&flux)
                .arg(&sediment_flux)
                .arg(&mut next_delta)
                .arg(&mut next_water)
                .arg(&mut next_sediment)
                .arg(&mut exported_water)
                .arg(&mut exported_sediment)
                .arg(&mut eroded)
                .arg(&mut deposited)
                .arg(&p)
                .launch(launch)?;
        }
        std::mem::swap(&mut delta, &mut next_delta);
        std::mem::swap(&mut water, &mut next_water);
        std::mem::swap(&mut sediment, &mut next_sediment);
    }
    Ok(State {
        delta: stream.clone_dtoh(&delta)?,
        water: stream.clone_dtoh(&water)?,
        sediment: stream.clone_dtoh(&sediment)?,
        exported_water: stream.clone_dtoh(&exported_water)?,
        exported_sediment: stream.clone_dtoh(&exported_sediment)?,
        eroded: stream.clone_dtoh(&eroded)?,
        deposited: stream.clone_dtoh(&deposited)?,
    })
}
fn initialize() -> Result<Runtime> {
    // Probe before calling cudarc's lazy loader, which otherwise panics when
    // CUDA/NVRTC is absent. Auto mode then returns a normal CPU fallback.
    if !unsafe { cudarc::driver::sys::is_culib_present() }
        || !unsafe { cudarc::nvrtc::sys::is_culib_present() }
    {
        bail!("CUDA driver or NVRTC unavailable; check CUDA Toolkit library search path");
    }
    let context = CudaContext::new(0)?;
    let ptx = compile_ptx_with_opts(
        SOURCE,
        CompileOptions {
            fmad: Some(false),
            ..Default::default()
        },
    )?;
    let module = context.load_module(ptx)?;
    Ok(Runtime {
        context,
        flux: module.load_function("compute_flux")?,
        update: module.load_function("update")?,
    })
}

const SOURCE: &str = r#"
struct Params { unsigned width,height; float spacing,dt,rain,conductance,capacity,erosion,deposition,max_erosion; };
__device__ bool boundary(unsigned i,Params p) {
    unsigned x=i%p.width,y=i/p.width;
    return x==0 || x+1==p.width || y==0 || y+1==p.height;
}
__device__ float neighbour_bed(unsigned i,unsigned j,const float* base,const float* delta,
    const unsigned char* fixed,Params p) {
    return base[j]+(boundary(j,p) && !fixed[j]?delta[i]:delta[j]);
}
extern "C" __global__ void compute_flux(const float* base,const unsigned char* fixed,const float* delta,
    const float* water,const float* sediment,float* flux,float* sf,Params p) {
    unsigned i=blockIdx.x*blockDim.x+threadIdx.x;
    if(i>=p.width*p.height || boundary(i,p)) return;
    unsigned ns[4]={i-1,i+1,i-p.width,i+p.width};
    float available=water[i]+p.rain,head=base[i]+delta[i]+available,sum=0;
    for(int k=0;k<4;k++) {
        unsigned j=ns[k];float rain=boundary(j,p)?0:p.rain;
        float drop=fmaxf(head-(neighbour_bed(i,j,base,delta,fixed,p)+water[j]+rain),0);
        flux[4*i+k]=p.conductance*drop;
        sum+=flux[4*i+k];
    }
    float scale=sum>0?fminf(0.9f*available/sum,1):0;
    for(int k=0;k<4;k++) {
        flux[4*i+k]*=scale;
        sf[4*i+k]=available>1e-12f?flux[4*i+k]*sediment[i]/available:0;
    }
}
extern "C" __global__ void update(const float* base,const unsigned char* fixed,
    const float* delta,const float* water,const float* sediment,const float* flux,const float* sf,
    float* nd,float* nw,float* ns,float* ew,float* es,float* eroded,float* deposited,Params p) {
    unsigned i=blockIdx.x*blockDim.x+threadIdx.x;
    if(i>=p.width*p.height) return;
    unsigned x=i%p.width,y=i/p.width;
    float iw=0,is=0;
    if(x>0) { iw+=flux[4*(i-1)+1];is+=sf[4*(i-1)+1]; }
    if(x+1<p.width) { iw+=flux[4*(i+1)];is+=sf[4*(i+1)]; }
    if(y>0) { iw+=flux[4*(i-p.width)+3];is+=sf[4*(i-p.width)+3]; }
    if(y+1<p.height) { iw+=flux[4*(i+p.width)+2];is+=sf[4*(i+p.width)+2]; }
    nd[i]=delta[i];
    if(boundary(i,p)) { ew[i]+=iw;es[i]+=is;nw[i]=0;ns[i]=0;return; }
    float ow=0,os=0;
    for(int k=0;k<4;k++) { ow+=flux[4*i+k];os+=sf[4*i+k]; }
    nw[i]=fmaxf(water[i]+p.rain+iw-ow,0);
    float solid=fmaxf(sediment[i]+is-os,0);
    if(!fixed[i]) {
        unsigned nb[4]={i-1,i+1,i-p.width,i+p.width};float h=base[i]+delta[i],slope=0;
        for(int k=0;k<4;k++) slope=fmaxf(slope,fmaxf((h-neighbour_bed(i,nb[k],base,delta,fixed,p))/p.spacing,0));
        float capacity=p.capacity*fmaxf(ow,iw)*slope;
        if(solid>capacity) {float d=(solid-capacity)*p.deposition*p.dt;nd[i]+=d;solid-=d;deposited[i]+=d;}
        else {float e=fminf((capacity-solid)*p.erosion*p.dt,p.max_erosion);nd[i]-=e;solid+=e;eroded[i]+=e;}
    }
    ns[i]=solid;
}
"#;
